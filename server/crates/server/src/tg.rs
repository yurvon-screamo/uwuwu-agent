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

    /// Пин токен-маскировки (#1150, High ревью раунда 1): Display reqwest
    /// печатает полный URL с токеном в пути — транспортные ошибки обязаны
    /// уходить в лог только в виде краткой классификации describe().
    #[tokio::test]
    async fn transport_errors_never_carry_token_or_url() {
        // порт 1 — соединение всегда отказано (частый класс на Railway)
        let c = TelegramClient::with_base("http://127.0.0.1:1", "sekret-token".into());
        let err = c
            .get_updates(None, 0)
            .await
            .expect_err("connect to closed port must fail");
        let msg = err.to_string();
        assert!(!msg.contains("sekret-token"), "токен утёк в ошибку: {msg}");
        assert!(!msg.contains("/bot"), "URL с токеном утёк в ошибку: {msg}");
    }

    /// Пин семантики ошибок (ревью раунда 1, Common): ТГ-неок (включая
    /// 409 «два поллера») — Telegram-вариант с видимым статусом, не Board.
    #[tokio::test]
    async fn non_ok_response_maps_to_telegram_variant() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            // читаем запрос целиком (заголовки + тело по Content-Length),
            // иначе ранний close даёт RST и reqwest падает на отправке
            let mut data = Vec::new();
            let mut buf = [0u8; 2048];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => data.extend_from_slice(&buf[..n]),
                }
                if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    let cl: usize = data[..pos]
                        .split(|b| *b == b'\n')
                        .find_map(|line| {
                            String::from_utf8_lossy(line)
                                .to_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    if data.len() >= pos + 4 + cl {
                        break;
                    }
                }
            }
            let body =
                r#"{"ok":false,"description":"Conflict: terminated by other getUpdates request"}"#;
            let resp = format!(
                "HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        });
        let c = TelegramClient::with_base(&format!("http://127.0.0.1:{port}"), "tok".into());
        let err = c
            .get_updates(None, 0)
            .await
            .expect_err("non-ok response must be an error");
        assert!(
            matches!(err, UsecaseError::Telegram(_)),
            "ожидал Telegram-вариант, получено: {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("409"), "статус 409 потерян в ошибке: {msg}");
    }
}
