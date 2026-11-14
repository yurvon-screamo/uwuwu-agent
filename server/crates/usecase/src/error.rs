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
}

impl fmt::Display for UsecaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Board(m) => write!(f, "доска: {m}"),
            Self::Session(m) => write!(f, "сессия: {m}"),
            Self::Pr(m) => write!(f, "pr: {m}"),
            Self::State(m) => write!(f, "состояние: {m}"),
        }
    }
}

impl std::error::Error for UsecaseError {}

pub type UsecaseResult<T> = Result<T, UsecaseError>;
