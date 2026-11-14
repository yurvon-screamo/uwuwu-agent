//! Query-юзкейсы (чтения для MCP-инструментов): тонкая оркестрация
//! портов без мутаций. Презентация (MCP) зовёт только их.

use crate::error::UsecaseError;
use crate::ports::{IssueView, TaskBoardRepo, WikiRepo};
use std::sync::Arc;
use uwuwu_domain::Task;

/// Отказ чтения: нет такой доски/дока — честное «не найдено»,
/// транспортные отказы — контекст источника.
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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(what) => write!(f, "не найдено: {what}"),
            Self::Source(e) => write!(f, "{e}"),
        }
    }
}

/// Сколько последних комментов несёт карточка задачи.
const CARD_COMMENTS: usize = 10;

pub struct TaskView {
    board: Arc<dyn TaskBoardRepo>,
    boards: Vec<(String, String)>, // (project, board)
}

impl TaskView {
    pub fn new(board: Arc<dyn TaskBoardRepo>, boards: Vec<(String, String)>) -> Self {
        Self { board, boards }
    }

    pub fn projects(&self) -> Vec<String> {
        self.boards.iter().map(|(p, _)| p.clone()).collect()
    }

    fn board_of(&self, project: &str) -> Option<&str> {
        self.boards
            .iter()
            .find(|(p, _)| p == project)
            .map(|(_, b)| b.as_str())
    }

    /// Список строк доски проекта (сводка).
    pub async fn list(&self, project: &str) -> Result<Vec<Task>, ViewError> {
        let board = self
            .board_of(project)
            .ok_or_else(|| ViewError::NotFound(format!("проект «{project}» вне конфига")))?;
        Ok(self.board.read_board(board).await?)
    }

    /// Карточка задачи: агрегат + тело + хвост комментов.
    pub async fn card(&self, project: &str, number: u64) -> Result<IssueView, ViewError> {
        let board = self
            .board_of(project)
            .ok_or_else(|| ViewError::NotFound(format!("проект «{project}» вне конфига")))?;
        let view = self.board.read_issue(board, number).await?;
        if view.task.status == uwuwu_domain::Status::Empty && view.body.is_empty() {
            return Err(ViewError::NotFound(format!("задача #{number}")));
        }
        Ok(IssueView {
            comments: view
                .comments
                .into_iter()
                .rev()
                .take(CARD_COMMENTS)
                .rev()
                .collect(),
            ..view
        })
    }
}

pub struct DocsView {
    wiki: Arc<dyn WikiRepo>,
}

impl DocsView {
    pub fn new(wiki: Arc<dyn WikiRepo>) -> Self {
        Self { wiki }
    }

    /// markdown дока по слагу (docs/**, без .md).
    pub async fn read(&self, slug: &str) -> Result<String, ViewError> {
        let safe = sanitize_slug(slug);
        self.wiki
            .read_doc(&safe)
            .await?
            .ok_or_else(|| ViewError::NotFound(format!("док «{slug}»")))
    }

    /// Пути .md под префиксом.
    pub async fn list(&self, prefix: &str) -> Result<Vec<String>, ViewError> {
        Ok(self.wiki.list_docs(&sanitize_slug(prefix)).await?)
    }
}

/// Персона владельца: компактный док, ходит в каждый спавн.
pub struct PersonView {
    wiki: Arc<dyn WikiRepo>,
}

/// Слаг персоны (канон: docs/Experience/person/PERSON.md).
pub const PERSON_SLUG: &str = "Experience/person/PERSON";

impl PersonView {
    pub fn new(wiki: Arc<dyn WikiRepo>) -> Self {
        Self { wiki }
    }

    pub async fn show(&self) -> Result<String, ViewError> {
        self.wiki
            .read_doc(PERSON_SLUG)
            .await?
            .ok_or_else(|| ViewError::NotFound("персона (PERSON)".into()))
    }
}

/// Слаг-гигиена: без ведущего docs/, без ../, без .md-суффикса.
fn sanitize_slug(slug: &str) -> String {
    let mut s = slug.trim().trim_start_matches("docs/").trim_matches('/');
    if let Some(stripped) = s.strip_suffix(".md") {
        s = stripped;
    }
    let parts: Vec<&str> = s
        .split('/')
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .collect();
    parts.join("/")
}
