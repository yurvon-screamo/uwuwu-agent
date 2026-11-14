//! Порты юзкейс-слоя: требования к инфре. Сигнатуры — на доменных типах
//! и нативных ошибках (UsecaseError); инфра (uwuwu-server) реализует их
//! поверх gh/опенкода/ФС.

use crate::error::UsecaseResult;
use async_trait::async_trait;
use uwuwu_domain::{Slot, Status, Task, UtcMinute};

/// Доска задач: чтение строк + мутации носителя (поля+fm синхронны —
/// инвариант «fm ≡ поля» держит реализация).
#[async_trait]
pub trait TaskBoardRepo: Send + Sync {
    async fn read_board(&self, board: &str) -> UsecaseResult<Vec<Task>>;
    async fn set_status(&self, board: &str, number: u64, status: Status) -> UsecaseResult<()>;
    /// Заявка сессии в слот (claim/спавн-привязка).
    async fn set_session(
        &self,
        board: &str,
        number: u64,
        slot: Slot,
        session: &str,
    ) -> UsecaseResult<()>;
    async fn post_comment(&self, number: u64, body: &str) -> UsecaseResult<()>;
    /// Закрыть issue (Done по канону = closed).
    async fn close_issue(&self, number: u64) -> UsecaseResult<()>;
    /// Хвост комментов задачи (для wakeup: новые против последнего).
    async fn latest_comments(&self, number: u64) -> UsecaseResult<Vec<CommentRef>>;
}

/// Коммент задачи в терминах wakeup: идентичность + время создания.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentRef {
    pub id: String,
    pub created_ms: u64,
    pub body: String,
}

/// Ссылка на сессию опенкода исполнителя.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSpec {
    /// cwd сессии на исполнителе (мастер-чекаут или env задачи).
    pub directory: String,
    pub title: String,
    /// Кастомный агент (developer/plan-reviewer/code-quality-reviewer/tester).
    pub agent: Option<String>,
    /// Модель спавна (provider, id); None — дефолт исполнителя.
    pub model: Option<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Alive,
    Dead,
    Unknown,
}

#[async_trait]
pub trait AgentSessionGateway: Send + Sync {
    async fn create_session(&self, spec: &SessionSpec) -> UsecaseResult<String>;
    async fn prompt(&self, session: &str, text: &str) -> UsecaseResult<()>;
    /// Slash-команда сессии (детерминированные shell-блоки — env-подготовка).
    async fn run_command(&self, session: &str, name: &str, args: &str) -> UsecaseResult<()>;
    async fn liveness(&self, session: &str) -> Liveness;
    async fn delete_session(&self, session: &str) -> UsecaseResult<()>;
    /// ms последней активности сессии (последнее сообщение) или None.
    async fn last_activity_ms(&self, session: &str) -> UsecaseResult<Option<u64>>;
}

/// Состояние PR проекта: источник вердиктов автодона.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    /// PR не найден ни по ячейке, ни по ветке.
    Missing,
    Open {
        mergeable: bool,
        ci_green: bool,
    },
    Merged,
    Closed,
}

#[async_trait]
pub trait PrGateway: Send + Sync {
    /// Проба PR: по ячейке (URL) либо по head-ветке.
    async fn probe(
        &self,
        project_repo: &str,
        pr_url: Option<&str>,
        branch: Option<&str>,
    ) -> UsecaseResult<PrState>;
    /// Мерж PR (merge-commit).
    async fn merge(&self, project_repo: &str, pr_url: &str) -> UsecaseResult<()>;
}

/// Часы: домен считает минуту UTC; оркестрация — эпоху.
pub trait Clock: Send + Sync {
    fn utc_minute(&self) -> UtcMinute;
    fn now_ms(&self) -> u64;
}

/// Персистентное состояние тика (wakeup-дедуп): носитель — дело инфры.
pub trait TickStateStore: Send + Sync {
    fn last_comment_id(&self, board: &str, number: u64) -> Option<String>;
    fn set_last_comment_id(&self, board: &str, number: u64, id: &str);
    fn save(&self) -> UsecaseResult<()>;
}
