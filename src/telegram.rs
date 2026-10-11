use crate::messaging::split_text;
use std::time::Duration;
use teloxide::{
    payloads::*,
    prelude::*,
    types::{
        AllowedUpdate, CallbackQueryId, InlineKeyboardButton, InlineKeyboardMarkup,
        LinkPreviewOptions, MessageId,
    },
};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Outgoing {
    pub chat: i64,
    pub text: String,
    pub approval: Option<String>,
    pub progress: Option<String>,
    pub callback: Option<String>,
    pub clear: Option<i32>,
}
pub struct Receipt {
    pub token: String,
    pub message: Option<i32>,
}

pub async fn poll(bot: Bot, tx: mpsc::Sender<Update>) {
    let mut offset = 0;
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
                    if tx.send(update).await.is_err() {
                        return;
                    }
                    offset = next;
                }
            }
            Err(error) => {
                let seconds = retry_delay(&error, delay);
                tracing::warn!("Telegram polling unavailable; retrying in {seconds}s");
                tokio::time::sleep(Duration::from_secs(seconds)).await;
                delay = (delay * 2).min(30);
            }
        }
    }
}
pub fn retry_delay(error: &teloxide::RequestError, fallback: u64) -> u64 {
    match error {
        teloxide::RequestError::RetryAfter(seconds) => u64::from(seconds.seconds()).max(1),
        _ => fallback,
    }
}
pub async fn deliver(bot: Bot, mut rx: mpsc::Receiver<Outgoing>, receipts: mpsc::Sender<Receipt>) {
    let mut progress_messages = std::collections::HashMap::new();
    while let Some(message) = rx.recv().await {
        if let Some(id) = message.callback {
            let _ = bot
                .answer_callback_query(CallbackQueryId(id))
                .text(&message.text)
                .await;
            continue;
        }
        if let Some(id) = message.clear {
            let _ = bot
                .edit_message_reply_markup(ChatId(message.chat), MessageId(id))
                .reply_markup(InlineKeyboardMarkup::default())
                .await;
            continue;
        }
        let chunks = split_text(&message.text, 3800);
        let mut delivered = None;
        let mut failed = false;
        for (index, text) in chunks.iter().enumerate() {
            for attempt in 0..5 {
                let result = if let Some(id) = message
                    .progress
                    .as_ref()
                    .and_then(|key| progress_messages.get(key))
                    .copied()
                {
                    bot.edit_message_text(ChatId(message.chat), MessageId(id), text)
                        .await
                } else {
                    let mut request = bot
                        .send_message(ChatId(message.chat), text)
                        .link_preview_options(LinkPreviewOptions {
                            is_disabled: true,
                            url: None,
                            prefer_small_media: false,
                            prefer_large_media: false,
                            show_above_text: false,
                        });
                    if index + 1 == chunks.len() {
                        if let Some(token) = &message.approval {
                            request = request.reply_markup(InlineKeyboardMarkup::new([[
                                InlineKeyboardButton::callback("Approve", format!("a:{token}")),
                                InlineKeyboardButton::callback("Deny", format!("d:{token}")),
                            ]]));
                        }
                    }
                    request.await
                };
                match result {
                    Ok(sent) => {
                        delivered = Some(sent.id.0);
                        if let Some(key) = &message.progress {
                            progress_messages.insert(key.clone(), sent.id.0);
                        }
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("Telegram delivery failed (content and token omitted)");
                        tokio::time::sleep(Duration::from_secs(retry_delay(&e, 1 << attempt)))
                            .await;
                        if attempt == 4 {
                            failed = true;
                        }
                    }
                }
            }
            if failed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1100)).await;
        }
        if let Some(token) = message.approval {
            let _ = receipts
                .send(Receipt {
                    token,
                    message: if failed { None } else { delivered },
                })
                .await;
        }
    }
}
