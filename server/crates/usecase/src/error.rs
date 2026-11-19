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
            Self::SessionNotFound(m) => write!(f, "сессия не найдена (dead-session): {m}"),
        }
    }
}

impl std::error::Error for UsecaseError {}

pub type UsecaseResult<T> = Result<T, UsecaseError>;
