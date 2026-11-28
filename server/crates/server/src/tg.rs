//! Telegram Bot API транспорт (реализация TelegramGateway): два метода —
//! getUpdates (long-poll) и sendMessage. Токен бота — в инстансе,
//! сообщения в лог не пишутся (приватность, #1150).

use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;
use uwuwu_usecase::bots::{MediaKind, TelegramGateway, TgMedia, TgMessage, TgUpdate};
use uwuwu_usecase::{UsecaseError, UsecaseResult};

const API_BASE: &str = "https://api.telegram.org";
/// Оверрайд базы ТГ-API (мок-стенд e2e)
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
                .timeout(Duration::from_secs(
                    uwuwu_usecase::runtime_cfg::bots::tg_http_timeout_secs(),
                ))
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

    /// Апдейт ТГ → TgUpdate: message и edited_message — обе ветки доезжают
    /// в буфер ввода (#1596); прочие типы — без сообщения, поглощаются.
    /// Индексация через Value (Null на отсутствующих ключах), НЕ через
    /// &Map: прямой m["chat"] на Map паникует «no entry found for key» —
    /// ядовитый апдейт без chat/from (посты каналов, edited_*) убивал
    /// бот-задачу каждые тик рестарт-лупом (инцидент 16.11).
    fn parse_update(raw: &Value) -> TgUpdate {
        let parse = |m: &Value| TgMessage {
            message_id: m["message_id"].as_i64().unwrap_or_default(),
            chat_type: m["chat"]["type"].as_str().unwrap_or_default().to_string(),
            chat_id: m["chat"]["id"].as_i64().unwrap_or_default(),
            from_username: m["from"]["username"].as_str().map(String::from),
            text: m["text"].as_str().map(String::from),
            caption: m["caption"].as_str().map(String::from),
            media: Self::parse_media(m),
            forward: Self::parse_forward(m),
        };
        TgUpdate {
            update_id: raw["update_id"].as_u64().unwrap_or_default(),
            message: raw.get("message").map(parse),
            edited_message: raw.get("edited_message").map(parse),
        }
    }

    /// Форвард-факт (#1416): Bot API `forward_origin` — 4 типа
    /// (user/hidden_user/chat/channel), каждый сворачивается в
    /// человекочитаемую метку источника для маркера промпта бот-сессии.
    /// Имя пользователя/псевдоним hidden едет в метку (рекомендация
    /// план-ревью #1416); author_signature канала не читается — это
    /// автор поста, не источник пересылки (грань в Границах задачи).
    /// Legacy-поля (forward_from, пре-Bot API 7.0) не читаются: живой
    /// форвард приходит только через forward_origin.
    fn parse_forward(m: &Value) -> Option<String> {
        let origin = &m["forward_origin"];
        match origin["type"].as_str()? {
            "user" => {
                let sender = &origin["sender_user"];
                Some(match sender["username"].as_str() {
                    Some(u) => format!("юзер @{u}"),
                    None => format!(
                        "юзер {}",
                        sender["first_name"].as_str().unwrap_or("без имени")
                    ),
                })
            }
            "hidden_user" => Some(match origin["sender_user_name"].as_str() {
                Some(n) => format!("скрытый источник ({n})"),
                None => "скрытый источник".to_string(),
            }),
            "chat" => Self::chat_forward_label(&origin["sender_chat"], "чат"),
            "channel" => Self::chat_forward_label(&origin["chat"], "канал"),
            _ => None,
        }
    }

    /// Метка chat/channel-источника: username предпочтительнее тайтла
    /// (короткий и стабильный); без обоих — форвард не размечается.
    fn chat_forward_label(chat: &Value, kind: &str) -> Option<String> {
        if let Some(u) = chat["username"].as_str() {
            return Some(format!("{kind} @{u}"));
        }
        chat["title"].as_str().map(|t| format!("{kind} «{t}»"))
    }

    /// Media-факт сообщения (#1171): photo — массив PhotoSize, последний
    /// элемент — максимальный размер; document — с именем/mime/размером;
    /// прочие не-текстовые вложения (video/voice/audio/animation/
    /// video_note/sticker) — Other (факт для явного отказа раннера).
    fn parse_media(m: &Value) -> Option<TgMedia> {
        if let Some(sizes) = m["photo"].as_array() {
            return sizes.last().map(|p| TgMedia {
                kind: MediaKind::Photo,
                file_id: p["file_id"].as_str().unwrap_or_default().to_string(),
                file_name: None,
                file_size: p["file_size"].as_i64(),
                mime: Some("image/jpeg".to_string()),
            });
        }
        if let Some(kind) = [
            "document",
            "video",
            "video_note",
            "voice",
            "audio",
            "animation",
            "sticker",
        ]
        .into_iter()
        .find(|k| m[k].is_object())
        {
            let v = &m[kind];
            let k = match kind {
                "document" => MediaKind::Document,
                _ => MediaKind::Other,
            };
            return Some(TgMedia {
                kind: k,
                file_id: v["file_id"].as_str().unwrap_or_default().to_string(),
                file_name: v["file_name"].as_str().map(String::from),
                file_size: v["file_size"].as_i64(),
                mime: v["mime_type"].as_str().map(String::from),
            });
        }
        None
    }

    /// Перманентный отказ getFile на файл > 20 MiB (`400 file is too
    /// big`) — отдельный типизированный маршрут, не общий сбой (#1171).
    /// Матч по строке описания Bot API: формулировка стабильна годами;
    /// при смене маршрут деградирует до общего «не смог забрать» (отказ
    /// остаётся явным) — пин ревью #398; формат держит юнит
    /// get_file_too_big_error_is_typed.
    fn classify_download_err(e: UsecaseError) -> UsecaseError {
        if e.to_string().contains("file is too big") {
            UsecaseError::TelegramTooBig(e.to_string())
        } else {
            e
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
                    "allowed_updates": ["message", "edited_message"],
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

    /// getFile → `GET /file/bot<token>/<file_path>` (#1171). Токен в пути
    /// обоих запросов — любые ошибки только через describe() (NFT-04).
    /// oversize: getFile сам отвечает 400 «file is too big» на файлы
    /// > 20 MiB — маршрутизируется в TelegramTooBig (не общий сбой).
    async fn download_file(&self, file_id: &str) -> UsecaseResult<Vec<u8>> {
        let info = match self.call("getFile", json!({ "file_id": file_id })).await {
            Ok(v) => v,
            Err(e) => return Err(Self::classify_download_err(e)),
        };
        let path = info["file_path"].as_str().unwrap_or_default();
        if path.is_empty() {
            return Err(UsecaseError::Telegram(format!(
                "tg getFile: нет file_path (file_id {file_id})"
            )));
        }
        let resp = self
            .http
            .get(format!("{}/file/bot{}/{}", self.base, self.token, path))
            .send()
            .await
            .map_err(|e| UsecaseError::Telegram(format!("tg file: {}", Self::describe(&e))))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| UsecaseError::Telegram(format!("tg file body: {}", Self::describe(&e))))?;
        if !status.is_success() {
            let body = String::from_utf8_lossy(&bytes).to_string();
            let err = UsecaseError::Telegram(format!("tg file: HTTP {status} {body}"));
            return Err(Self::classify_download_err(err));
        }
        Ok(bytes.to_vec())
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
        assert!(u.edited_message.is_none());
    }

    /// #1596: правка сообщения — полноценный вход буфера с id оригинала.
    #[test]
    fn parses_edited_message_with_message_id() {
        let raw = json!({
            "update_id": 19,
            "edited_message": {
                "message_id": 77,
                "from": {"id": 1, "username": "yurvon"},
                "chat": {"id": 42, "type": "private"},
                "text": "исправленный текст"
            }
        });
        let u = TelegramClient::parse_update(&raw);
        assert!(u.message.is_none());
        let m = u.edited_message.expect("правка распарсена");
        assert_eq!(m.message_id, 77);
        assert_eq!(m.text.as_deref(), Some("исправленный текст"));
        assert_eq!(m.chat_id, 42);
    }

    /// Инцидент 16.11: ядовитый edited-апдейт без chat/from не должен
    /// паниковать парсер и убивать бот-задачу (#1596).
    #[test]
    fn poison_edited_update_absorbed_without_panic() {
        let raw = json!({"update_id": 20, "edited_message": {"message_id": 78}});
        let u = TelegramClient::parse_update(&raw);
        assert_eq!(u.update_id, 20);
        let m = u.edited_message.expect("форма распарсена");
        assert_eq!(m.message_id, 78);
        assert_eq!(m.chat_type, "");
        assert_eq!(m.chat_id, 0);
        assert_eq!(m.from_username, None);
    }

    /// #1171: photo-апдейт — массив PhotoSize, берём максимальный
    /// (последний), caption в media-сообщении — не text.
    #[test]
    fn parses_photo_update_with_largest_size_and_caption() {
        let raw = json!({
            "update_id": 9,
            "message": {
                "from": {"username": "yurvon"},
                "chat": {"id": 42, "type": "private"},
                "caption": "от МПНЧ",
                "photo": [
                    {"file_id": "small", "file_size": 1000},
                    {"file_id": "large", "file_size": 99999}
                ]
            }
        });
        let u = TelegramClient::parse_update(&raw);
        let m = u.message.expect("message");
        assert!(m.text.is_none());
        assert_eq!(m.caption.as_deref(), Some("от МПНЧ"));
        let media = m.media.expect("media");
        assert_eq!(media.kind, MediaKind::Photo);
        assert_eq!(media.file_id, "large", "берём максимальный размер");
        assert_eq!(media.file_size, Some(99999));
        assert_eq!(media.file_name, None);
        assert_eq!(media.mime.as_deref(), Some("image/jpeg"));
    }

    /// #1171: document — file_name/mime/size доезжают в media-факт.
    #[test]
    fn parses_document_update_with_metadata() {
        let raw = json!({
            "update_id": 10,
            "message": {
                "from": {"username": "yurvon"},
                "chat": {"id": 42, "type": "private"},
                "document": {
                    "file_id": "doc1",
                    "file_name": "отчёт МПНЧ.pdf",
                    "mime_type": "application/pdf",
                    "file_size": 500_000
                }
            }
        });
        let m = TelegramClient::parse_update(&raw).message.expect("message");
        let media = m.media.expect("media");
        assert_eq!(media.kind, MediaKind::Document);
        assert_eq!(media.file_name.as_deref(), Some("отчёт МПНЧ.pdf"));
        assert_eq!(media.mime.as_deref(), Some("application/pdf"));
        assert_eq!(media.file_size, Some(500_000));
    }

    /// #1171: не-текстовые вложения вне photo/document (видео/стикер/…)
    /// — факты presence с kind=Other, чтобы раннер отказал явно, не тихо.
    #[test]
    fn other_media_classified_as_other_kind() {
        let raw = json!({
            "update_id": 11,
            "message": {
                "from": {"username": "yurvon"},
                "chat": {"id": 42, "type": "private"},
                "video": {"file_id": "vid1", "file_size": 42}
            }
        });
        let m = TelegramClient::parse_update(&raw).message.expect("message");
        let media = m.media.expect("media");
        assert_eq!(media.kind, MediaKind::Other);
        assert_eq!(media.file_id, "vid1");
    }

    /// Чисто текстовое сообщение — media нет (регресс поля).
    #[test]
    fn text_message_has_no_media() {
        let raw = json!({
            "update_id": 12,
            "message": {
                "from": {"username": "yurvon"},
                "chat": {"id": 42, "type": "private"},
                "text": "привет"
            }
        });
        let m = TelegramClient::parse_update(&raw).message.expect("message");
        assert!(m.media.is_none());
        assert!(m.caption.is_none());
    }

    /// #1416: forward_origin.channel — метка «канал @username»
    /// (username предпочтительнее тайтла).
    #[test]
    fn parses_channel_forward_origin_username_label() {
        let raw = json!({
            "update_id": 13,
            "message": {
                "from": {"username": "yurvon"},
                "chat": {"id": 42, "type": "private"},
                "text": "пост про nginx",
                "forward_origin": {
                    "type": "channel",
                    "chat": {"id": -100123, "type": "channel", "title": "Nginx Gems", "username": "nginx"}
                }
            }
        });
        let m = TelegramClient::parse_update(&raw).message.expect("message");
        assert_eq!(m.forward.as_deref(), Some("канал @nginx"));
    }

    /// #1416: user-origin — username в метку; без username — имя
    /// (Bot API first_name, рекомендация план-ревью); hidden_user —
    /// псевдоним, который ТГ показывает получателю.
    #[test]
    fn parses_user_and_hidden_forward_origins() {
        let with_username = json!({
            "update_id": 14,
            "message": {
                "from": {"username": "yurvon"},
                "chat": {"id": 42, "type": "private"},
                "text": "x",
                "forward_origin": {
                    "type": "user",
                    "sender_user": {"id": 7, "first_name": "Иван", "username": "ivanov"}
                }
            }
        });
        let m = TelegramClient::parse_update(&with_username)
            .message
            .expect("message");
        assert_eq!(m.forward.as_deref(), Some("юзер @ivanov"));

        let without_username = json!({
            "update_id": 15,
            "message": {
                "from": {"username": "yurvon"},
                "chat": {"id": 42, "type": "private"},
                "text": "x",
                "forward_origin": {
                    "type": "user",
                    "sender_user": {"id": 8, "first_name": "Пётр"}
                }
            }
        });
        let m = TelegramClient::parse_update(&without_username)
            .message
            .expect("message");
        assert_eq!(m.forward.as_deref(), Some("юзер Пётр"));

        let hidden = json!({
            "update_id": 16,
            "message": {
                "from": {"username": "yurvon"},
                "chat": {"id": 42, "type": "private"},
                "text": "x",
                "forward_origin": {"type": "hidden_user", "sender_user_name": "Кто-то"}
            }
        });
        let m = TelegramClient::parse_update(&hidden)
            .message
            .expect("message");
        assert_eq!(m.forward.as_deref(), Some("скрытый источник (Кто-то)"));
    }

    /// #1416: chat-origin — fallback на тайтл группы без username;
    /// MessageOriginChat несёт чат в поле sender_chat.
    #[test]
    fn parses_chat_forward_origin_title_fallback() {
        let raw = json!({
            "update_id": 17,
            "message": {
                "from": {"username": "yurvon"},
                "chat": {"id": 42, "type": "private"},
                "text": "x",
                "forward_origin": {
                    "type": "chat",
                    "sender_chat": {"id": -100999, "type": "supergroup", "title": "Рабочий чат"}
                }
            }
        });
        let m = TelegramClient::parse_update(&raw).message.expect("message");
        assert_eq!(m.forward.as_deref(), Some("чат «Рабочий чат»"));
    }

    /// #1416: не-форвард — forward пуст (регресс поля): обычный текст
    /// не размечается маркером.
    #[test]
    fn plain_message_has_no_forward() {
        let raw = json!({
            "update_id": 18,
            "message": {
                "from": {"username": "yurvon"},
                "chat": {"id": 42, "type": "private"},
                "text": "привет"
            }
        });
        let m = TelegramClient::parse_update(&raw).message.expect("message");
        assert!(m.forward.is_none());
    }

    /// Перманентный oversize-отказ getFile — типизированный маршрут
    /// TelegramTooBig (не общий Telegram-сбой), по строке описания API.
    #[test]
    fn get_file_too_big_error_is_typed() {
        let plain =
            UsecaseError::Telegram("tg getFile: HTTP 400 Bad Request: file is too big".into());
        assert!(matches!(
            TelegramClient::classify_download_err(plain),
            UsecaseError::TelegramTooBig(_)
        ));
        let other = UsecaseError::Telegram("tg getFile: HTTP 500".into());
        assert!(matches!(
            TelegramClient::classify_download_err(other),
            UsecaseError::Telegram(_)
        ));
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

    /// #1596: без edited_message в allowed_updates правки не приедут —
    /// пин на состав запроса, а не только на парсер.
    #[tokio::test]
    async fn get_updates_requests_edited_messages() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
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
            let _ = tx.send(data);
            let body = r#"{"ok":true,"result":[]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        });
        let c = TelegramClient::with_base(&format!("http://127.0.0.1:{port}"), "tok".into());
        let updates = c.get_updates(None, 0).await.expect("getUpdates ok");
        assert!(updates.is_empty());
        let request = rx.recv().expect("запрос перехвачен");
        let request = String::from_utf8_lossy(&request);
        assert!(
            request.contains("edited_message"),
            "allowed_updates без правок: {request}"
        );
    }
}
