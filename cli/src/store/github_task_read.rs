//! GitHubStore read-side trait-impl: list/resolve/body/discover + делегация
//! write-side (github_task_write.rs). Структура/конвертация — github_task.rs.

use super::github_task::GitHubStore;
use super::task_store::{parse_reply_marker, CreateTask, EditTask, StoreResult, TaskStore, ThreadEntry};
use super::{StoreError, Task, TaskBoard};
use crate::store::github_gh::gh_json;
use serde_json::Value;

#[async_trait::async_trait]
impl TaskStore for GitHubStore {
    async fn list(&self, board: &TaskBoard) -> StoreResult<Vec<Task>> {
        // Явный query-string (URL-encoded colon): gh -f/-F форма на GET
        // давала искажённую выборку (живой e2e 13.11 — свежий issue не
        // попадал в -f labels, ручной query-string видел).
        let label = crate::store::github_task::urlencoded(&self.label_of(board));
        // Полная выборка с пагинацией (контракт трейта): uwuwu-доска
        // превышает первую страницу 100 (332 items).
        let mut all_rows: Vec<Value> = Vec::new();
        let mut page = 1usize;
        loop {
            let data = gh_json(
                &[
                    "api",
                    &format!(
                        "repos/{}/issues?labels={label}&state=open&per_page=100&page={page}",
                        self.repo
                    ),
                ],
                None,
            )
            .await?;
            let Some(arr) = data.as_array() else {
                break;
            };
            let fetched = arr.len();
            all_rows.extend(arr.iter().cloned());
            if fetched < 100 {
                break;
            }
            page += 1;
        }
        // Status — канон Pv2: батч-джойн статусов доски (GraphQL, пагинация
        // внутри). Пустая карта (канбана нет) — Todo-дефолт в convert.
        let statuses = super::github_project_resolve::project_statuses(&board.project).await?;
        let mut out = Vec::new();
        for issue in &all_rows {
            if issue.get("pull_request").is_some() {
                continue;
            }
            let number = issue["number"].to_string();
            out.push(self.convert(issue, statuses.get(&number).map(String::as_str)));
        }
        Ok(out)
    }

    async fn resolve(&self, board: &TaskBoard, needle: &str) -> Result<Task, StoreError> {
        let tasks = self.list(board).await?;
        let exact = needle.trim_start_matches('-');
        if let Some(t) = tasks.iter().find(|t| t.id == exact) {
            return Ok(t.clone());
        }
        let matched: Vec<&Task> = tasks
            .iter()
            .filter(|t| t.title.to_lowercase().contains(&needle.to_lowercase()))
            .collect();
        match matched.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(StoreError::ListCheckedButNotFound {
                needle: needle.to_string(),
            }),
            many => Err(StoreError::Other(anyhow::anyhow!(
                "задача «{needle}»: неоднозначно на {} строках ({}) — уточни номер issue",
                many.len(),
                many.iter()
                    .map(|t| format!("#{} «{}»", t.id, t.title))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    async fn body_get(&self, _board: &TaskBoard, task: &Task) -> StoreResult<String> {
        self.issue_body(&task.id).await
    }

    /// Полная перезапись тела: мета в md не живёт (ADR-098) — тело пишется
    /// как есть; `base` — снапшот свежести (сырое тело из get).
    async fn body_replace(
        &self,
        _board: &TaskBoard,
        task: &Task,
        new_md: &str,
        base: &str,
    ) -> StoreResult<Task> {
        let current = self.issue_body(&task.id).await?;
        if current != base {
            anyhow::bail!(
                "тело изменилось с момента снапшота (--base) — перечитай task get и повтори"
            );
        }
        self.set_issue_body(&task.id, new_md).await?;
        Ok(self.issue_fresh(&task.id).await?)
    }

    /// Тред = issue comments (плоский список; reply-семантика — цитатный
    /// маркер, вложенности нет). Маркер «> ответ на <значение>» первой
    /// строкой (его пишет comment_impl) читается как reply_to — структура
    /// ответов видна в thread-выводе; текст коммента не режется.
    async fn thread(&self, _board: &TaskBoard, task: &Task) -> StoreResult<Vec<ThreadEntry>> {
        let number = &task.id;
        let mut out = Vec::new();
        let mut page = 1usize;
        loop {
            let data = gh_json(
                &[
                    "api",
                    "-X",
                    "GET",
                    &format!("repos/{}/issues/{number}/comments", self.repo),
                    "-F",
                    "per_page=100",
                    "-F",
                    &format!("page={page}"),
                ],
                None,
            )
            .await?;
            let Some(items) = data.as_array() else {
                break;
            };
            let fetched = items.len();
            for c in items {
                let text = c["body"].as_str().unwrap_or("").to_string();
                out.push(ThreadEntry {
                    id: c["id"].to_string(),
                    reply_to: parse_reply_marker(&text),
                    author: c
                        .pointer("/user/login")
                        .and_then(|l| l.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    text,
                });
            }
            if fetched < 100 {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    async fn body_attrs(
        &self,
        _board: &TaskBoard,
        _task: &Task,
    ) -> StoreResult<std::collections::HashMap<String, String>> {
        Ok(Default::default())
    }

    async fn discover_boards(&self) -> StoreResult<Vec<TaskBoard>> {
        let labels = gh_json(&["api", &format!("repos/{}/labels", self.repo)], None).await?;
        let mut out = Vec::new();
        if let Value::Array(items) = labels {
            for l in &items {
                let name = l["name"].as_str().unwrap_or("");
                if let Some(project) = name.strip_prefix("proj:") {
                    out.push(TaskBoard {
                        project: project.to_string(),
                    });
                }
            }
        }
        Ok(out)
    }

    async fn create(&self, board: &TaskBoard, input: &CreateTask) -> Result<Task, StoreError> {
        self.create_impl(board, input).await
    }

    async fn edit(&self, board: &TaskBoard, task: &Task, patch: &EditTask) -> StoreResult<Task> {
        self.edit_impl(board, task, patch).await
    }

    async fn comment(
        &self,
        board: &TaskBoard,
        task: &Task,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<super::task_store::CommentInsert, StoreError> {
        self.comment_impl(board, task, text, reply_to).await
    }

    async fn claim(&self, board: &TaskBoard, task: &Task, session: &str) -> StoreResult<Task> {
        self.claim_impl(board, task, session).await
    }

    async fn close(&self, board: &TaskBoard, task: &Task, note: Option<&str>) -> StoreResult<Task> {
        self.close_impl(board, task, note).await
    }

    async fn set_body_attr(
        &self,
        board: &TaskBoard,
        task: &Task,
        key: &str,
        value: &str,
    ) -> StoreResult<()> {
        self.set_body_attr_impl(board, task, key, value).await
    }
}

#[cfg(test)]
mod fm_compose_tests {
    // fm-обвязка тела снесена (ADR-098: мета не в md, задача 1087).
    // Контракт body_replace — чистая перезапись, покрытие — процессные
    // контракт-тесты hub_cli_contract_process_tests.
}
