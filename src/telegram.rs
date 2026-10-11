use crate::messaging::split_text;
use std::time::Duration;
use teloxide::{
    payloads::*,
    prelude::*,
    types::{AllowedUpdate, LinkPreviewOptions},
};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct Outgoing {
    pub chat: i64,
    pub text: String,
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
pub async fn deliver(bot: Bot, mut rx: mpsc::Receiver<Outgoing>) {
    while let Some(message) = rx.recv().await {
        for text in split_text(&message.text, 3800) {
            for attempt in 0..5 {
                let result = bot
                    .send_message(ChatId(message.chat), &text)
                    .link_preview_options(LinkPreviewOptions {
                        is_disabled: true,
                        url: None,
                        prefer_small_media: false,
                        prefer_large_media: false,
                        show_above_text: false,
                    })
                    .await;
                match result {
                    Ok(_) => break,
                    Err(e) => {
                        tracing::warn!("Telegram delivery failed (content and token omitted)");
                        tokio::time::sleep(Duration::from_secs(retry_delay(&e, 1 << attempt)))
                            .await;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(1100)).await;
        }
    }
}
