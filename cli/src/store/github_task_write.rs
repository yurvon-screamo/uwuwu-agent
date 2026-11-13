//! GitHubStore write-side: create/edit/comment/claim/close/attrs.
//! Хелперы-методы (единообразие с SiYuanStore: один trait-impl с
//! делегацией — E0119). Мутации Project v2 полей — github_project.rs.

use super::github_project;
use super::github_task::{Frontmatter, GitHubStore};
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
        let mut fm_cells: Vec<(&str, String)> = vec![("status", status.clone())];
        if let Some(pr) = &input.priority {
            fm_cells.push(("priority", pr.clone()));
        }
        if let Some(ad) = input.autodone {
            fm_cells.push(("autodone", if ad { "true" } else { "false" }.to_string()));
        }
        let fm_pairs: Vec<(&str, &str)> = fm_cells.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let body = Frontmatter::parse(&body_md).with_fields(&fm_pairs);
        let mut payload = Map::new();
        payload.insert("title".into(), json!(input.title));
        payload.insert("body".into(), json!(body));
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
        github_project::set_status_and_planned(self, &number, Some(&status), None).await?;
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
        let front_cells: Vec<(&str, &String)> = [
            ("branch", &patch.branch),
            ("pr", &patch.pr),
            ("dev-session", &patch.dev_session),
            ("priority", &patch.priority),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.as_ref().map(|s| (k, s)))
        .collect();
        if !front_cells.is_empty() {
            let body_md = self.issue_body(&number).await?;
            let cells: Vec<(&str, &str)> =
                front_cells.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let body = Frontmatter::parse(&body_md).with_fields(&cells);
            self.set_issue_body(&number, &body).await?;
        }
        if let Some(ad) = patch.autodone {
            let body_md = self.issue_body(&number).await?;
            let body = Frontmatter::parse(&body_md)
                .with_field("autodone", if ad { "true" } else { "false" });
            self.set_issue_body(&number, &body).await?;
        }
        if let Some(status) = &patch.status {
            github_project::set_status_and_planned(self, &number, Some(status), None).await?;
        }
        if let Some((planned, _ms)) = &patch.planned {
            github_project::set_status_and_planned(self, &number, None, Some(planned)).await?;
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
        // маркер + текст, честный паритет SiYuan «вложенный пункт»
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
        let body_md = self.issue_body(&task.id).await?;
        let body = Frontmatter::parse(&body_md).with_field("dev-session", session);
        self.set_issue_body(&task.id, &body).await?;
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

    pub(super) async fn set_body_attr_impl(
        &self,
        _board: &TaskBoard,
        task: &Task,
        key: &str,
        value: &str,
    ) -> StoreResult<()> {
        let body_md = self.issue_body(&task.id).await?;
        let body = Frontmatter::parse(&body_md).with_field(key, value);
        self.set_issue_body(&task.id, &body).await
    }
}
