//! Backend-слой uwuwu-cli: контракты хранилища (TaskStore, DocStore) и
//! нейтральные типы. Реализация — `github_task` (uwuwu-hub; срезы B/C
//! задачи 20261112155302-ttyp35g).

pub mod doc_store;
pub mod github_doc_blob;
pub mod github_doc_clone;
pub mod github_doc_search;
pub mod github_doc_store;
pub mod github_gh;
pub mod github_issue_fields;
pub mod github_project;
pub mod github_project_resolve;
pub mod github_task;
pub mod github_task_read;
pub mod github_task_write;
pub mod git_util;
pub mod task_migrate;
pub mod task_store;

#[cfg(test)]
mod store_tests;
#[cfg(test)]
mod github_doc_clone_tests;

pub use doc_store::{DocEntry, DocStore};
pub use task_store::{CommentInsert, CreateTask, EditTask, TaskStore, ThreadEntry};

/// Фабрика: GitHubStore над UWUWU_HUB_REPO (единственный бекенд; утилита
/// для cmd-путей и будущих потребителей store-слоя).
pub fn task_store(repo: String) -> std::sync::Arc<dyn TaskStore> {
    std::sync::Arc::new(github_task::GitHubStore::new(repo))
}

use std::fmt;

/// Доска задач проекта: нейтральный указатель (GitHub — label).
#[derive(Clone, Debug)]
pub struct TaskBoard {
    pub project: String,
}

/// Таск-домен: тип из канона доски ("" — не задан). Валидация значений —
/// store-реализация (GitHub issue types).
#[derive(Clone, Debug)]
pub struct TaskType(pub String);

impl TaskType {
    pub fn is_set(&self) -> bool {
        !self.0.is_empty()
    }
}

/// Строка задачи в нейтральной форме: проекция SyTask и GitHub issue.
/// Поля-дубли доски (Branch/PR/Сессия dev/Запланировано) на GitHub живут
/// в body frontmatter — store-реализация их читает/пишет сама.
#[derive(Clone, Debug)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub status: String,
    pub task_type: TaskType,
    pub branch: String,
    pub pr: String,
    pub dev_session: String,
    /// Дисплей-дата «Запланировано» ("" — пусто).
    pub planned: String,
    /// Машиночитаемая дата той же клетки, epoch ms (0 — пусто).
    pub planned_ms: i64,
    /// Отображение расписания крона (schedule мастера).
    pub schedule_cell: String,
    /// Приоритет (канон CUP-очереди: high/medium/low, "" — не задан).
    pub priority: String,
    /// Autodone: галочка автомержа PR при входе в Review.
    pub autodone: bool,
}

/// Ошибка store-операции: классы, по которым CLI/демоны ветвят поведение
/// (дубль-детект create; «не найдено» — резолв-фоллбеки). Остальное — Other.
#[derive(Debug)]
pub enum StoreError {
    /// create: живой матч по тайтлу в скоупе доски — громкий отказ.
    DuplicateTitle {
        title: String,
        id: String,
    },
    NotFound {
        needle: String,
    },
    /// GitHub search API лагает (eventual consistency): дубль-резолв ретрая
    /// должен идти list-ом — реализация сигнализирует, что list пробован.
    ListCheckedButNotFound {
        needle: String,
    },
    /// Остальные сбои бекенда (транспорт, валидации ядра/API).
    Other(anyhow::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateTitle { title, id } => {
                write!(f, "дубль тайтла «{title}» — живая строка {id}")
            }
            Self::NotFound { needle } => write!(f, "не найдено: {needle}"),
            Self::ListCheckedButNotFound { needle } => {
                write!(f, "list-резолв пробован — не найдено: {needle}")
            }
            Self::Other(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<anyhow::Error> for StoreError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}
