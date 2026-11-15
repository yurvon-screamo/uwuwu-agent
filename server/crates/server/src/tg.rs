//! Telegram Bot API транспорт (реализация TelegramGateway): два метода —
//! getUpdates (long-poll) и sendMessage. Токен бота — в инстансе,
//! сообщения в лог не пишутся (приватность, #1150).

use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;
use uwuwu_usecase::bots::{TelegramGateway, TgMessage, TgUpdate};
use uwuwu_usecase::{UsecaseError, UsecaseResult};

const API_BASE: &str = "https://api.telegram.org";
/// Оверрайд базы ТГ-API (мок-стенд e2e) — паритет UWUWU_EMBED_URL.
const ENV_TG_API: &str = "UWUWU_TG_API";

pub struct TelegramClient {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl TelegramClient {
    /// Таймаут клиента — long-poll (25с) + запас на ответ сети.
    pub fn new(token: String) -> Self {
        Self::with_base(&Self::api_base(), token)
    }

    fn api_base() -> String {
        std::env::var(ENV_TG_API)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| API_BASE.to_string())
    }

    fn with_base(base: &str, token: String) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            token,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(35))
                .build()
                .expect("reqwest client"),
        }
    }

    /// Краткая классификация транспортной ошибки БЕЗ URL: reqwest в
    /// Display печатает полный url запроса, а в пути — токен бота
    /// (NFT-04: токен не попадает в логи).
    fn describe(e: &reqwest::Error) -> String {
        if e.is_timeout() {
            return "timeout".into();
        }
        if e.is_connect() {
            return "connect failed".into();
        }
        if e.is_decode() {
            return "response decode failed".into();
        }
        if e.is_request() {
            return "request build failed".into();
        }
        match e.status() {
            Some(s) => format!("http {s}"),
            None => "transport".into(),
        }
    }

    async fn call(&self, method: &str, body: Value) -> UsecaseResult<Value> {
        let resp = self
            .http
            .post(format!("{}/bot{}/{}", self.base, self.token, method))
            .json(&body)
            .send()
            .await
            .map_err(|e| UsecaseError::Telegram(format!("tg {method}: {}", Self::describe(&e))))?;
        let status = resp.status();
        let value: Value = resp.json().await.map_err(|e| {
            UsecaseError::Telegram(format!("tg {method} body: {}", Self::describe(&e)))
        })?;
        if !status.is_success() || value["ok"].as_bool() != Some(true) {
            let desc = value["description"].as_str().unwrap_or_default();
            // 409 «два поллера» и прочий ТГ-неок — Telegram-класс ошибки
            return Err(UsecaseError::Telegram(format!(
                "tg {method}: HTTP {status} {desc}"
            )));
        }
        Ok(value["result"].clone())
    }

    /// Апдейт ТГ → TgUpdate (не-message типы — без сообщения).
    fn parse_update(raw: &Value) -> TgUpdate {
        TgUpdate {
            update_id: raw["update_id"].as_u64().unwrap_or_default(),
            message: raw["message"].as_object().map(|m| TgMessage {
                chat_type: m["chat"]["type"].as_str().unwrap_or_default().to_string(),
                chat_id: m["chat"]["id"].as_i64().unwrap_or_default(),
                from_username: m["from"]["username"].as_str().map(String::from),
                text: m["text"].as_str().map(String::from),
            }),
        }
    }
}

#[async_trait]
impl TelegramGateway for TelegramClient {
    async fn get_updates(
        &self,
        offset: Option<u64>,
        timeout_sec: u64,
    ) -> UsecaseResult<Vec<TgUpdate>> {
        let result = self
            .call(
                "getUpdates",
                json!({
                    "offset": offset,
                    "timeout": timeout_sec,
                    "allowed_updates": ["message"],
                }),
            )
            .await?;
        Ok(result
            .as_array()
            .map(|list| list.iter().map(Self::parse_update).collect())
            .unwrap_or_default())
    }

    async fn send_message(&self, chat_id: i64, text: &str) -> UsecaseResult<()> {
        self.call("sendMessage", json!({ "chat_id": chat_id, "text": text }))
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_base_trims_trailing_slash() {
        let c = TelegramClient::with_base("http://127.0.0.1:8098/", "tok".into());
        assert_eq!(c.base, "http://127.0.0.1:8098");
    }

    #[test]
    fn parses_message_update() {
        let raw = json!({
            "update_id": 7,
            "message": {
                "message_id": 1,
                "from": {"id": 1, "username": "yurvon"},
                "chat": {"id": 42, "type": "private"},
                "text": "привет"
            }
        });
        let u = TelegramClient::parse_update(&raw);
        assert_eq!(u.update_id, 7);
        let m = u.message.unwrap();
        assert_eq!(m.chat_type, "private");
        assert_eq!(m.chat_id, 42);
        assert_eq!(m.from_username.as_deref(), Some("yurvon"));
        assert_eq!(m.text.as_deref(), Some("привет"));
    }

    #[test]
    fn non_message_update_absorbed() {
        let raw = json!({"update_id": 8, "callback_query": {"id": "x"}});
        let u = TelegramClient::parse_update(&raw);
        assert_eq!(u.update_id, 8);
        assert!(u.message.is_none());
    }
}
