use std::collections::VecDeque;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::config::TelegramConfig;
use crate::wechat_bot::{Contact, WeChatBot};

// ============================================
// Telegram Bot API types
// ============================================

#[derive(Deserialize, Debug)]
struct TgResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(Deserialize, Debug)]
struct TgUser {
    #[allow(dead_code)]
    id: i64,
    first_name: String,
    last_name: Option<String>,
    username: Option<String>,
}

#[derive(Deserialize, Debug)]
struct TgChat {
    id: i64,
    #[serde(rename = "type")]
    chat_type: String,
}

#[derive(Deserialize, Debug)]
struct TgMessage {
    from: Option<TgUser>,
    chat: TgChat,
    text: Option<String>,
}

#[derive(Deserialize, Debug)]
struct TgUpdate {
    update_id: i64,
    message: Option<TgMessage>,
}

#[derive(Serialize)]
struct SendMessageRequest {
    chat_id: String,
    text: String,
}

// ============================================
// TelegramBot
// ============================================

pub struct TelegramBot {
    api_base: String,
    client: Client,
    offset: i64,
    buffer: VecDeque<(Contact, String)>,
}

impl TelegramBot {
    pub fn new(cfg: &TelegramConfig) -> Self {
        Self {
            api_base: format!("https://api.telegram.org/bot{}", cfg.bot_token),
            client: Client::new(),
            offset: 0,
            buffer: VecDeque::new(),
        }
    }
}

#[async_trait]
impl WeChatBot for TelegramBot {
    async fn start(&mut self) -> Result<()> {
        let url = format!("{}/getMe", self.api_base);
        let resp: TgResponse<TgUser> = self
            .client
            .get(&url)
            .send()
            .await
            .context("Telegram getMe failed. Check your bot_token and network.")?
            .json()
            .await
            .context("Failed to parse getMe response")?;

        if !resp.ok {
            anyhow::bail!(
                "Telegram bot token invalid: {}",
                resp.description.unwrap_or_default()
            );
        }

        let me = resp.result.context("No user in getMe response")?;
        info!(
            "Telegram bot online: @{} ({})",
            me.username.unwrap_or_default(),
            me.first_name
        );
        Ok(())
    }

    async fn recv_message(&mut self) -> Result<Option<(Contact, String)>> {
        // Drain buffer first
        if let Some(msg) = self.buffer.pop_front() {
            return Ok(Some(msg));
        }

        // Long-poll getUpdates
        loop {
            let url = format!(
                "{}/getUpdates?offset={}&timeout=30&allowed_updates=[\"message\"]",
                self.api_base, self.offset
            );

            let resp: TgResponse<Vec<TgUpdate>> = self
                .client
                .get(&url)
                .timeout(Duration::from_secs(35))
                .send()
                .await
                .context("getUpdates request failed")?
                .json()
                .await
                .context("getUpdates parse failed")?;

            if !resp.ok {
                anyhow::bail!(
                    "getUpdates failed: {}",
                    resp.description.unwrap_or_default()
                );
            }

            let updates = resp.result.unwrap_or_default();
            if updates.is_empty() {
                continue; // timeout, poll again
            }

            for update in updates {
                self.offset = update.update_id + 1;

                let msg = match update.message {
                    Some(m) => m,
                    None => continue,
                };

                // Private messages only
                if msg.chat.chat_type != "private" {
                    debug!("Skipping non-private message from chat {}", msg.chat.id);
                    continue;
                }

                let text = match msg.text {
                    Some(ref t) if !t.is_empty() => t.clone(),
                    _ => continue,
                };

                let user = msg.from.unwrap_or(TgUser {
                    id: msg.chat.id,
                    first_name: "Unknown".into(),
                    last_name: None,
                    username: None,
                });

                let nickname = match &user.last_name {
                    Some(last) => format!("{} {}", user.first_name, last),
                    None => user.first_name.clone(),
                };

                let contact = Contact {
                    wxid: msg.chat.id.to_string(),
                    nickname,
                    remark_name: user.username.unwrap_or_default(),
                };

                self.buffer.push_back((contact, text));
            }

            if let Some(msg) = self.buffer.pop_front() {
                return Ok(Some(msg));
            }
        }
    }

    async fn send_message(&self, contact: &Contact, message: &str) -> Result<()> {
        let url = format!("{}/sendMessage", self.api_base);
        let body = SendMessageRequest {
            chat_id: contact.wxid.clone(),
            text: message.to_string(),
        };

        let resp: TgResponse<serde_json::Value> = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("sendMessage request failed")?
            .json()
            .await
            .context("sendMessage parse failed")?;

        if !resp.ok {
            anyhow::bail!(
                "sendMessage failed: {}",
                resp.description.unwrap_or_default()
            );
        }
        Ok(())
    }
}
