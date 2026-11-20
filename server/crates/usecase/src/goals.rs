//! Реестр целей проекта: issues hub-репо с лейблами `goal` +
//! `proj:<проект>` (#1216). Носитель — GitHub issues: open = в работе,
//! closed = достигнута; пишет владелец в UI, агент читает перед работой
//! над задачей (правило AGENTS.md).

use crate::error::ViewError;
use async_trait::async_trait;
use std::sync::Arc;

/// Одна открытая цель: номер и тайтл issue носителя + первая
/// содержательная строка тела (суть).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalIssue {
    pub number: u64,
    pub title: String,
    pub gist: String,
}

/// Порт чтения целей — носитель скрыт от юзкейса (домен не знает
/// носитель: сегодня issues GitHub, завтра — что решит владелец).
#[async_trait]
pub trait GoalsRepo: Send + Sync {
    /// Открытые цели проекта (лейблы goal + proj:<проект>).
    async fn open_goals(&self, project: &str) -> crate::UsecaseResult<Vec<GoalIssue>>;
}

/// Первая содержательная строка тела — суть цели; пустое тело → пустой
/// gist. Длинная строка обрезается до MAX_GIST_GLYPHS глифов с «…».
pub fn gist_of(body: &str) -> String {
    const MAX_GIST_GLYPHS: usize = 120;
    let line = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() <= MAX_GIST_GLYPHS {
        return line.to_string();
    }
    let mut cut: String = line.chars().take(MAX_GIST_GLYPHS).collect();
    cut.push('…');
    cut
}

/// Форматер реестра целей — контракт MCP goal_list. Проекты конвейера
/// и URL фильтра (носитель) приходят снаружи: домен не строит адрес.
pub struct GoalsView {
    repo: Arc<dyn GoalsRepo>,
    /// Проекты конвейера (конфиг сервера).
    projects: Vec<String>,
    /// Шаблон URL фильтра open-целей в UI носителя, `{project}` —
    /// плейсхолдер имени проекта.
    filter_url_tpl: String,
}

impl GoalsView {
    pub fn new(repo: Arc<dyn GoalsRepo>, projects: Vec<String>, filter_url_tpl: String) -> Self {
        Self {
            repo,
            projects,
            filter_url_tpl,
        }
    }

    /// Markdown-реестр целей проекта: «- #N title — суть». Пусто →
    /// подсказка про носитель и URL фильтра. Проект вне конфига —
    /// честный NotFound (паттерн TaskView::list).
    pub async fn list(&self, project: &str) -> Result<String, ViewError> {
        if !self.projects.iter().any(|p| p == project) {
            return Err(ViewError::NotFound(format!(
                "проект «{project}» вне конфига конвейера"
            )));
        }
        let goals = self.repo.open_goals(project).await?;
        if goals.is_empty() {
            return Ok(format!(
                "цели проекта не заведены (носитель: issues hub, лейблы \
                 goal + proj:{project}; ведёт владелец в UI: {})",
                self.filter_url_tpl.replace("{project}", project)
            ));
        }
        Ok(goals
            .iter()
            .map(|g| {
                if g.gist.is_empty() {
                    format!("- #{} {}", g.number, g.title)
                } else {
                    format!("- #{} {} — {}", g.number, g.title, g.gist)
                }
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

#[cfg(test)]
#[path = "goals_tests.rs"]
mod goals_tests;
