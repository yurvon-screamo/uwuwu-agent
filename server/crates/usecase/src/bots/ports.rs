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

/// Сообщение ТГ: только факты маршрутизации, текст — не для логов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TgMessage {
    /// «private» | «group» | «supergroup» | «channel».
    pub chat_type: String,
    pub chat_id: i64,
    /// Username отправителя без @; None — не задан у пользователя.
    pub from_username: Option<String>,
    pub text: Option<String>,
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
