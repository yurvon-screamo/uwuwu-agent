//! Нативные ошибки юзкейс-слоя: типизированные контексты отказов
//! портов/оркестрации. Никаких багов-сумок (anyhow) — конкретный вид
//! отказа виден в типе; динамическая деталь живёт в сообщении.

use std::fmt;

/// Контексты отказов по источнику; деталь — в сообщении.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsecaseError {
    /// Доска задач (чтение строк, мутации носителя).
    Board(String),
    /// Исполнитель сессий (опенкод).
    Session(String),
    /// PR-шлюз (пробы/мерж).
    Pr(String),
    /// Персистентное состояние тика.
    State(String),
    /// Telegram Bot API (боты, #1150).
    Telegram(String),
    /// Файл превышает потолок скачивания Bot API (20 MiB) — перманентный
    /// отказ `getFile`, ретрай бессмысленен (#1171).
    TelegramTooBig(String),
    /// Blob-канал (#1292): отказ S3-носителя (сеть/статус) или ввода.
    Blob(String),
    /// Файл превышает потолок blob-канала (20 MiB, прецедент UC-06).
    BlobTooBig(String),
    /// Blob-канал не сконфигурирован: не заданы env `UWUWU_S3_*` (#1292).
    BlobNotConfigured(String),
    /// Дедуп-ключ blob не прошёл доменный гард `BlobKey` (#1292).
    BlobBadKey(String),
}

impl fmt::Display for UsecaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Board(m) => write!(f, "доска: {m}"),
            Self::Session(m) => write!(f, "сессия: {m}"),
            Self::Pr(m) => write!(f, "pr: {m}"),
            Self::State(m) => write!(f, "состояние: {m}"),
            Self::Telegram(m) => write!(f, "telegram: {m}"),
            Self::TelegramTooBig(m) => write!(f, "telegram too big: {m}"),
            Self::Blob(m) => write!(f, "blob: {m}"),
            Self::BlobTooBig(m) => write!(f, "blob too big: {m}"),
            Self::BlobNotConfigured(m) => write!(f, "blob не сконфигурирован: {m}"),
            Self::BlobBadKey(m) => write!(f, "blob bad key: {m}"),
        }
    }
}

impl std::error::Error for UsecaseError {}

pub type UsecaseResult<T> = Result<T, UsecaseError>;
