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
    /// Исполнитель отказал в доставке: сессии нет или она недоставляема
    /// (dead-session класс, #1288) — финальная, не временная: ретрай той
    /// же сессии бессмысленен, маршрут — resurrection/парковка.
    SessionNotFound(String),
    /// Blob-канал (#1292): отказ S3-носителя (сеть/статус) или ввода.
    Blob(String),
    /// Файл превышает потолок blob-канала (20 MiB, прецедент UC-06).
    BlobTooBig(String),
    /// Blob-канал не сконфигурирован: не заданы env `UWUWU_S3_*` (#1292).
    BlobNotConfigured(String),
    /// Дедуп-ключ blob не прошёл доменный гард `BlobKey` (#1292).
    BlobBadKey(String),
    /// Эмбеддинг-маршрут упёрся в лимиты провайдера (HTTP 429, #1363):
    /// временная — ретрай осмыслен в том же часу; бэкофф из RetryInfo.
    RateLimited {
        service: String,
        retry_after_secs: Option<u64>,
    },
}

impl fmt::Display for UsecaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Board(m) => write!(f, "board: {m}"),
            Self::Session(m) => write!(f, "session: {m}"),
            Self::Pr(m) => write!(f, "pr: {m}"),
            Self::State(m) => write!(f, "state: {m}"),
            Self::Telegram(m) => write!(f, "telegram: {m}"),
            Self::TelegramTooBig(m) => write!(f, "telegram too big: {m}"),
            Self::SessionNotFound(m) => write!(f, "session not found (dead-session): {m}"),
            Self::Blob(m) => write!(f, "blob: {m}"),
            Self::BlobTooBig(m) => write!(f, "blob too big: {m}"),
            Self::BlobNotConfigured(m) => write!(f, "blob is not configured: {m}"),
            Self::BlobBadKey(m) => write!(f, "blob bad key: {m}"),
            Self::RateLimited {
                service,
                retry_after_secs,
            } => match retry_after_secs {
                Some(s) => write!(f, "{service}: rate limited, retry after {s}s"),
                None => write!(f, "{service}: rate limited"),
            },
        }
    }
}

impl std::error::Error for UsecaseError {}

pub type UsecaseResult<T> = Result<T, UsecaseError>;

/// Отказ чтения (MCP-вьюхи): нет такой доски/дока — честное «не найдено»,
/// транспортные отказы — контекст источника. Нейтральный дом: тип общий
/// для вьюх задач и целей, не собственность неймспейса tasks (#1216).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewError {
    NotFound(String),
    Source(UsecaseError),
}

impl From<UsecaseError> for ViewError {
    fn from(e: UsecaseError) -> Self {
        Self::Source(e)
    }
}

impl std::fmt::Display for ViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(what) => write!(f, "not found: {what}"),
            Self::Source(e) => write!(f, "{e}"),
        }
    }
}
