//! GitHubStore write-side: create/edit/comment/claim/close/attrs.
//! Хелперы-методы (единый trait-impl с делегацией — E0119).
//! Клетки — глобальные issue fields (ADR-098), Status — Pv2 single-select
//! (github_project.rs). fm-шапка тел снесена (задача 1087).

use super::github_issue_fields::{self, names, FieldValue};
use super::github_project;
use super::github_task::GitHubStore;
use super::task_store::{CommentInsert, CreateTask, EditTask, StoreResult, TaskStore};
use super::{StoreError, Task, TaskBoard};
use crate::store::github_gh::gh_json;
use serde_json::{json, Map, Value};

impl GitHubStore {
    pub(super) async fn create_impl(
        &self,
        board: &TaskBoard,
        input: &CreateTask,
    ) -> Result<Task, StoreError> {
        // идемпотентность: list-first по label+тайтл (консистентный REST),
        // дубль — громкий отказ; search API не участвует (eventual
        // consistency индекса — вердикт ревью дельта-раунд 2)
        let existing = self.list(board).await?;
        let canon = input.title.to_lowercase();
        if let Some(t) = existing.iter().find(|t| t.title.to_lowercase() == canon) {
            return Err(StoreError::DuplicateTitle {
                title: input.title.clone(),
                id: t.id.clone(),
            });
        }
        let status = input.status.clone().unwrap_or_else(|| "Draft".into());
        let body_md = match &input.body_md {
            Some(md) => md.clone(),
            None => format!(
                "# {}\n\n\n<!-- task-type: {} -->\n",
                input.title, input.task_type.0
            ),
        };
        // тело пишется как есть: мета в md не живёт (ADR-098), дефолты
        // клеток (priority medium — решение владельца 13.11, autodone true —
        // 14.11) уходят в поля ниже
        let mut payload = Map::new();
        payload.insert("title".into(), json!(input.title));
        payload.insert("body".into(), json!(body_md));
        payload.insert("labels".into(), json!([self.label_of(board)]));
        if input.task_type.is_set() {
            payload.insert("type".into(), json!(input.task_type.0));
        }
        let created = match gh_json(
            &["api", "-X", "POST", &format!("repos/{}/issues", self.repo)],
            Some(&Value::Object(payload).to_string()),
        )
        .await
        {
            Ok(created) => created,
            Err(post_err) => {
                // POST упал (сеть/таймаут) — возможно, issue создан: пауза
                // ≥ лага платформы, ретрай-резолв по точному тайтлу через
                // list (консистентный REST); не найден — исходная ошибка
                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                let existing = self.list(board).await?;
                let canon = input.title.to_lowercase();
                if let Some(t) = existing.iter().find(|t| t.title.to_lowercase() == canon) {
                    return Ok(t.clone());
                }
                return Err(StoreError::Other(post_err));
            }
        };
        let number = created["number"].to_string();
        github_project::set_status(self, &number, Some(&status)).await?;
        let priority = input.priority.as_deref().unwrap_or("medium");
        let autodone = input.autodone.unwrap_or(true);
        github_issue_fields::set_fields(
            &number,
            &self.repo,
            &[
                (
                    names::PRIORITY,
                    Some(FieldValue::Select(priority.to_string())),
                ),
                (
                    names::AUTODONE,
                    Some(FieldValue::Select(
                        if autodone { "on" } else { "off" }.into(),
                    )),
                ),
            ],
        )
        .await?;
        Ok(self.issue_fresh(&number).await?)
    }

    pub(super) async fn edit_impl(
        &self,
        _board: &TaskBoard,
        task: &Task,
        patch: &EditTask,
    ) -> StoreResult<Task> {
        let number = task.id.clone();
        let mut payload = Map::new();
        if let Some(title) = &patch.title {
            payload.insert("title".into(), json!(title));
        }
        if let Some(ty) = &patch.task_type {
            payload.insert(
                "type".into(),
                match ty.is_set() {
                    true => json!(ty.0),
                    false => Value::Null,
                },
            );
        }
        if !payload.is_empty() {
            gh_json(
                &[
                    "api",
                    "-X",
                    "PATCH",
                    &format!("repos/{}/issues/{number}", self.repo),
                ],
                Some(&Value::Object(payload).to_string()),
            )
            .await?;
        }
        // Клетки — один батч в глобальные поля; Some("") — очистка клетки
        // (DELETE); статус — Pv2-мутация отдельно (github_project.rs).
        let text_cell = |v: &Option<String>| -> Option<Option<FieldValue>> {
            v.as_ref().map(|s| match s.as_str() {
                "" => None,
                other => Some(FieldValue::Text(other.to_string())),
            })
        };
        let mut cells: Vec<(&str, Option<FieldValue>)> = Vec::new();
        if let Some(v) = text_cell(&patch.branch) {
            cells.push((names::BRANCH, v));
        }
        if let Some(v) = text_cell(&patch.pr) {
            cells.push((names::PR, v));
        }
        if let Some(v) = text_cell(&patch.dev_session) {
            cells.push((names::DEV_SESSION, v));
        }
        if let Some(p) = &patch.priority {
            cells.push((
                names::PRIORITY,
                match p.as_str() {
                    "" => None,
                    other => Some(FieldValue::Select(other.to_string())),
                },
            ));
        }
        if let Some(ad) = patch.autodone {
            cells.push((
                names::AUTODONE,
                Some(FieldValue::Select(if ad { "on" } else { "off" }.into())),
            ));
        }
        if let Some((planned, _ms)) = &patch.planned {
            let date = super::github_project::normalize_date(planned)?;
            cells.push((
                names::PLANNED,
                match date.as_str() {
                    "" => None,
                    other => Some(FieldValue::Date(other.to_string())),
                },
            ));
        }
        if !cells.is_empty() {
            github_issue_fields::set_fields(&number, &self.repo, &cells).await?;
        }
        if let Some(status) = &patch.status {
            github_project::set_status(self, &number, Some(status)).await?;
        }
        self.issue_fresh(&number).await
    }

    pub(super) async fn comment_impl(
        &self,
        _board: &TaskBoard,
        task: &Task,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<CommentInsert, StoreError> {
        // issue comments — плоский список (тредов нет): reply = цитатный
        // маркер + текст, честный паритет вложенных пунктов
        let body = match reply_to {
            Some(anchor) => format!("> ответ на {anchor}\n\n{text}"),
            None => text.to_string(),
        };
        let number = &task.id;
        let created = gh_json(
            &[
                "api",
                "-X",
                "POST",
                &format!("repos/{}/issues/{number}/comments", self.repo),
            ],
            Some(&json!({ "body": body }).to_string()),
        )
        .await?;
        Ok(CommentInsert {
            id: created["id"].to_string(),
        })
    }

    pub(super) async fn claim_impl(
        &self,
        _board: &TaskBoard,
        task: &Task,
        session: &str,
    ) -> StoreResult<Task> {
        github_issue_fields::set_fields(
            &task.id,
            &self.repo,
            &[(
                names::DEV_SESSION,
                Some(FieldValue::Text(session.to_string())),
            )],
        )
        .await?;
        self.issue_fresh(&task.id).await
    }

    pub(super) async fn close_impl(
        &self,
        board: &TaskBoard,
        task: &Task,
        note: Option<&str>,
    ) -> StoreResult<Task> {
        if let Some(n) = note {
            self.comment_impl(board, task, &format!("закрыта: {n}"), None)
                .await?;
        }
        let number = &task.id;
        gh_json(
            &[
                "api",
                "-X",
                "PATCH",
                &format!("repos/{}/issues/{number}", self.repo),
            ],
            Some(&json!({ "state": "closed", "state_reason": "completed" }).to_string()),
        )
        .await?;
        self.issue_fresh(number).await
    }

    /// Атрибут тела: единственный потребитель — `--schedule` (клетка
    /// «Расписание»). Прочие ключи — громкий отказ (молчаливая запись в
    /// несуществующий носитель маскировала бы потерю).
    pub(super) async fn set_body_attr_impl(
        &self,
        _board: &TaskBoard,
        task: &Task,
        key: &str,
        value: &str,
    ) -> StoreResult<()> {
        if key != "schedule" {
            anyhow::bail!("set_body_attr: ключ «{key}» не поддержан (только schedule)");
        }
        github_issue_fields::set_fields(
            &task.id,
            &self.repo,
            &[(names::SCHEDULE, Some(FieldValue::Text(value.to_string())))],
        )
        .await?;
        Ok(())
    }
}
