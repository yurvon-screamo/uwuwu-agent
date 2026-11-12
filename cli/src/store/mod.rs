//! Backend-слой uwuwu-cli: контракты хранилища (TaskStore, DocStore) и
//! нейтральные типы. Реализации: `siyuan` (существующий контур) и `github`
//! (uwuwu-hub, срез C задачи 20261112155302-ttyp35g). Выбор — по env
//! `UWUWU_HUB_REPO` (задан → GitHub); контрактовые типы — проекция доменных
//! строк доски (SyTask) и issue uwuwu-hub в одну форму.

pub mod doc_store;
pub mod siyuan_task_store;
pub mod siyuan_task_store_write;
pub mod task_store;

#[cfg(test)]
mod store_tests;

pub use doc_store::{DocEntry, DocStore};
pub use task_store::{CommentInsert, CreateTask, EditTask, TaskStore, ThreadEntry};

use std::fmt;

/// Доска задач проекта: нейтральный указатель (SiYuan — база/av; GitHub — label).
#[derive(Clone, Debug)]
pub struct TaskBoard {
    pub project: String,
}

/// Таск-домен: тип из канона доски ("" — не задан). Валидация значений —
/// store-реализация (SiYuan validate_task_type / GitHub issue types).
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
    /// Отображение расписания крона (custom-schedule мастера).
    pub schedule_cell: String,
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
