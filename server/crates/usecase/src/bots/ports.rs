//! Порты бот-контура: ТГ-шлюз (long-poll + отправка) и персистентное
//! состояние бота (session/offset) в терминах супервизора и раннера.

use crate::error::UsecaseResult;
use async_trait::async_trait;

/// Апдейт ТГ (потребляются только `message`-апдейты).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TgUpdate {
    pub update_id: u64,
    /// None — не message-апдейт (edited_message и т.п.): поглощается.
    pub message: Option<TgMessage>,
}

/// Тип медиа-вложения ТГ (#1171): Photo/Document — качаются сервером,
/// Other (video/voice/sticker/…) — фиксируется фактом для явного отказа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Photo,
    Document,
    Other,
}

/// Media-факт сообщения: file_id для скачивания, метаданные для промпта
/// и гард-оверсайза. `file_name` — сырая строка юзера: нормализация до
/// построения пути — контракт BotAssetStore, не парсера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TgMedia {
    pub kind: MediaKind,
    pub file_id: String,
    pub file_name: Option<String>,
    /// Байты; ТГ отдаёт до 2^52 — i64 достаточен, Option — поле
    /// опциональное в API (тогда oversize ловится 400 от getFile).
    pub file_size: Option<i64>,
    pub mime: Option<String>,
}

/// Классифицированный вход бота: текст или media (с caption).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    Text(String),
    Media {
        caption: Option<String>,
        media: TgMedia,
    },
}

/// Сообщение ТГ: только факты маршрутизации, текст — не для логов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TgMessage {
    /// «private» | «group» | «supergroup» | «channel».
    pub chat_type: String,
    pub chat_id: i64,
    /// Username отправителя без @; None — не задан у пользователя.
    pub from_username: Option<String>,
    pub text: Option<String>,
    /// Подпись медиа-сообщения (у медиа вместо text, #1171).
    pub caption: Option<String>,
    /// Media-вложение; None — чисто текстовое/служебное сообщение.
    pub media: Option<TgMedia>,
}

#[async_trait]
pub trait TelegramGateway: Send + Sync {
    /// Long-poll апдейтов: блокирует до timeout сек. offset — последний
    /// обработанный update_id + 1 (None — с начала).
    async fn get_updates(
        &self,
        offset: Option<u64>,
        timeout_sec: u64,
    ) -> UsecaseResult<Vec<TgUpdate>>;
    async fn send_message(&self, chat_id: i64, text: &str) -> UsecaseResult<()>;
    /// Скачивание файла по file_id: getFile → GET file-эндпоинта. Файл
    /// больше 20 MiB — `UsecaseError::TelegramTooBig` (перманентный отказ
    /// API, ретрай бессмысленен, #1171).
    async fn download_file(&self, file_id: &str) -> UsecaseResult<Vec<u8>>;
}

/// Приёмник ассетов бота (#1171): файл → URL в приватном hub-репо,
/// адресуемый и владельцу (браузер-сессия), и агентам (gh). Реализация
/// владеет нормализацией имени (путь-компоненты/фолбэк/длина) и
/// построением percent-encoded ссылки.
#[async_trait]
pub trait BotAssetStore: Send + Sync {
    async fn put(
        &self,
        bot: &str,
        kind: MediaKind,
        filename: Option<&str>,
        bytes: &[u8],
    ) -> UsecaseResult<String>;
}

/// Персистентное состояние бота: вечная сессия + offset апдейтов
/// (state.json, секция bots). Сессия переживает stop/re-arm; смена
/// model/workspace — clear_bot_session + пересоздание раннером.
pub trait BotStateStore: Send + Sync {
    fn bot_session(&self, slug: &str) -> Option<String>;
    /// Some — записать, None — забыть (старая сессия в opencode.db живёт).
    fn set_bot_session(&self, slug: &str, session: Option<&str>);
    fn bot_offset(&self, slug: &str) -> Option<u64>;
    fn set_bot_offset(&self, slug: &str, offset: u64);
    /// Hash методички, вшитой в сессию бутстрап-промптом (doc-режим):
    /// расхождение = правка методички → следующий промпт понесёт её
    /// снова (один раз), дальше — чистый текст.
    fn bot_manual_hash(&self, slug: &str) -> Option<u64>;
    fn set_bot_manual_hash(&self, slug: &str, hash: u64);
    fn save(&self) -> UsecaseResult<()>;
}
