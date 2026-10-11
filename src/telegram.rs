use crate::{
    messaging::split_text,
    store::{Job, Store, unix_now},
};
use anyhow::{Context, Result};
use std::{collections::HashMap, time::Duration};
use teloxide::{
    ApiError, RequestError,
    payloads::*,
    prelude::*,
    types::{
        AllowedUpdate, CallbackQueryId, InlineKeyboardButton, InlineKeyboardMarkup,
        LinkPreviewOptions, MessageId,
    },
};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Outgoing {
    pub chat: i64,
    pub text: String,
    pub approval: Option<String>,
    pub progress: Option<String>,
    pub callback: Option<String>,
    pub clear: Option<i32>,
    pub expires: Option<i64>,
}
pub struct Receipt {
    pub token: String,
    pub message: Option<i32>,
}
pub struct Inbound {
    pub update: Update,
    pub ack: oneshot::Sender<()>,
}

pub async fn poll(bot: Bot, store: Store, tx: mpsc::Sender<Inbound>) -> Result<()> {
    let mut offset = store.offset()?;
    let mut delay = 1;
    loop {
        match bot
            .get_updates()
            .offset(offset)
            .timeout(25)
            .limit(20)
            .allowed_updates(vec![AllowedUpdate::Message, AllowedUpdate::CallbackQuery])
            .await
        {
            Ok(updates) => {
                delay = 1;
                for update in updates {
                    let next = update.id.0 as i32 + 1;
                    let (ack, done) = oneshot::channel();
                    if tx.send(Inbound { update, ack }).await.is_err() {
                        return Ok(());
                    }
                    if done.await.is_err() {
                        return Ok(());
                    }
                    offset = next;
                }
            }
            Err(error) => {
                if matches!(error, RequestError::Api(ApiError::InvalidToken)) {
                    anyhow::bail!("Telegram rejected bot authentication");
                }
                let seconds = retry_delay(&error, delay);
                tracing::warn!("Telegram polling unavailable; retrying in {seconds}s");
                tokio::time::sleep(Duration::from_secs(seconds)).await;
                delay = (delay * 2).min(30);
            }
        }
    }
}
pub fn retry_delay(error: &RequestError, fallback: u64) -> u64 {
    match error {
        RequestError::RetryAfter(seconds) => u64::from(seconds.seconds()).max(1),
        _ => fallback,
    }
}
pub fn retryable(error: &RequestError) -> bool {
    matches!(
        error,
        RequestError::RetryAfter(_)
            | RequestError::Network(_)
            | RequestError::Api(ApiError::Unknown(_))
    )
}
fn previews() -> LinkPreviewOptions {
    LinkPreviewOptions {
        is_disabled: true,
        url: None,
        prefer_small_media: false,
        prefer_large_media: false,
        show_above_text: false,
    }
}
async fn send_job(
    bot: &Bot,
    job: &Job,
    progress: &HashMap<String, i32>,
) -> std::result::Result<Option<i32>, RequestError> {
    let m = &job.message;
    if let Some(id) = &m.callback {
        bot.answer_callback_query(CallbackQueryId(id.clone()))
            .text(&m.text)
            .await?;
        return Ok(None);
    }
    if let Some(id) = m.clear {
        bot.edit_message_reply_markup(ChatId(m.chat), MessageId(id))
            .reply_markup(InlineKeyboardMarkup::default())
            .await?;
        return Ok(None);
    }
    let chunks = split_text(&m.text, 3800);
    let Some(text) = chunks.get(job.part) else {
        return Ok(None);
    };
    if let Some(id) = m.progress.as_ref().and_then(|key| progress.get(key)) {
        return match bot
            .edit_message_text(ChatId(m.chat), MessageId(*id), text)
            .link_preview_options(previews())
            .await
        {
            Ok(sent) => Ok(Some(sent.id.0)),
            Err(RequestError::Api(ApiError::MessageNotModified)) => Ok(Some(*id)),
            Err(e) => Err(e),
        };
    }
    let mut request = bot
        .send_message(ChatId(m.chat), text)
        .link_preview_options(previews());
    if job.part + 1 == chunks.len()
        && let Some(token) = &m.approval
    {
        request = request.reply_markup(InlineKeyboardMarkup::new([[
            InlineKeyboardButton::callback("Approve", format!("a:{token}")),
            InlineKeyboardButton::callback("Deny", format!("d:{token}")),
        ]]));
    }
    Ok(Some(request.await?.id.0))
}
pub async fn deliver(
    bot: Bot,
    store: Store,
    receipts: mpsc::Sender<Receipt>,
    users: std::collections::HashSet<u64>,
) -> Result<()> {
    let mut progress = HashMap::new();
    loop {
        let Some(job) = store.next_job()? else {
            if let Some(seconds) = store.next_wait()? {
                tokio::select! {_=store.notified()=>{},_=tokio::time::sleep(Duration::from_secs(seconds))=>{}}
            } else {
                store.notified().await;
            }
            continue;
        };
        let expired = job.message.expires.is_some_and(|t| t <= unix_now());
        let authorized = job.message.chat > 0 && users.contains(&(job.message.chat as u64));
        if expired || !authorized {
            store.delete(job.id)?;
            if let Some(token) = job.message.approval {
                receipts
                    .send(Receipt {
                        token,
                        message: None,
                    })
                    .await
                    .context("Receipt consumer stopped")?;
            }
            continue;
        }
        match send_job(&bot, &job, &progress).await {
            Ok(message) => {
                if let (Some(key), Some(id)) = (&job.message.progress, message) {
                    if progress.len() >= 256 {
                        progress.clear();
                    }
                    progress.insert(key.clone(), id);
                }
                if store.advance(&job)?
                    && let Some(token) = job.message.approval
                {
                    receipts
                        .send(Receipt { token, message })
                        .await
                        .context("Receipt consumer stopped")?;
                }
            }
            Err(error) => {
                if retryable(&error) {
                    let seconds = retry_delay(&error, (1u64 << job.attempts.min(6)).min(60));
                    store.retry(job.id, seconds)?;
                    // Telegram 429 applies globally; honor the entire retry_after interval.
                    if matches!(error, RequestError::RetryAfter(_)) {
                        tokio::time::sleep(Duration::from_secs(seconds)).await;
                    }
                    tracing::warn!(
                        job = job.id,
                        "Telegram delivery deferred; payload/token omitted"
                    );
                } else {
                    tracing::error!(
                        job = job.id,
                        "Telegram permanently rejected delivery; payload/token omitted"
                    );
                    store.dead_letter(job.id)?;
                    if let Some(token) = job.message.approval {
                        receipts
                            .send(Receipt {
                                token,
                                message: None,
                            })
                            .await
                            .context("Receipt consumer stopped")?;
                    }
                }
            }
        }
        // One outgoing request per 1.1s stays below per-chat and global bot limits.
        tokio::time::sleep(Duration::from_millis(1100)).await;
    }
}
