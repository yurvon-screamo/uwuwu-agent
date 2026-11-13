//! GitHubStore read-side trait-impl: list/resolve/body/discover + делегация
//! write-side (github_task_write.rs). Структура/конвертация — github_task.rs.

use super::github_task::GitHubStore;
use super::task_store::{CreateTask, EditTask, StoreResult, TaskStore, ThreadEntry};
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
        let data = Value::Array(all_rows);
        let mut out = Vec::new();
        if let Value::Array(items) = data {
            for issue in &items {
                if issue.get("pull_request").is_some() {
                    continue;
                }
                out.push(self.convert(issue));
            }
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

    /// Полная перезапись тела с сохранением fm-карточки (шапка
    /// `---\n…\n---`): если new_md не несёт собственный fm — переносится
    /// текущий; карточки не было — пустой блок не создаётся; new_md со
    /// своим fm пишется как есть (append-путь: current+md уже с fm).
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
        let composed = compose_body_with_fm(new_md, &current);
        self.set_issue_body(&task.id, &composed).await?;
        Ok(self.issue_fresh(&task.id).await?)
    }

    /// Тред = issue comments (плоский список; reply-семантика — цитатный
    /// маркер, вложенности нет).
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
                out.push(ThreadEntry {
                    id: c["id"].to_string(),
                    reply_to: None,
                    author: c
                        .pointer("/user/login")
                        .and_then(|l| l.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    text: c["body"].as_str().unwrap_or("").to_string(),
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

/// fm-карточка тела: `---\n…\n---\n` в начале строки (Option — карточки
/// может не быть: issue создан руками в UI без шапки).
fn fm_block(body: &str) -> Option<&str> {
    let rest = body.strip_prefix("---\n")?;
    let mut end = 4usize;
    for line in rest.split_inclusive('\n') {
        end += line.len();
        if line.trim_end() == "---" {
            return Some(&body[..end]);
        }
    }
    None
}

/// Сборка нового тела с сохранением fm-карточки. Правила:
/// - new_md с собственным fm — пишется как есть (append-путь передаёт
///   current+md: карточка уже внутри, повторный prepend задвоил бы блок —
///   High ревью хвоста 5);
/// - new_md без fm, карточка есть — fm переносится сверху (replace-путь:
///   контентное тело не затирает карточку статусов);
/// - карточки нет ни там, ни там — пустой `---\n---` не создаётся
///   (Common ревью: мусорный блок жил в rest_md навсегда).
fn compose_body_with_fm(new_md: &str, current: &str) -> String {
    if new_md.starts_with("---\n") {
        return new_md.to_string();
    }
    match fm_block(current) {
        Some(fm) => format!("{fm}\n{new_md}"),
        None => new_md.to_string(),
    }
}

#[cfg(test)]
mod fm_compose_tests {
    use super::{compose_body_with_fm, fm_block};

    const CARD: &str = "---\nstatus: In Progress\nbranch: work/x\n---\n\nтело задачи\n";

    #[test]
    fn append_не_задваивает_fm() {
        // append-путь: new_md = current + добавка (уже с карточкой)
        let new_md = format!("{CARD}добавка\n");
        let out = compose_body_with_fm(&new_md, CARD);
        assert_eq!(out, new_md, "тело с fm пишется как есть");
        assert_eq!(out.matches("---\n").count(), 2, "ровно один fm-блок");
    }

    #[test]
    fn replace_переносит_карточку_на_контент_без_fm() {
        let out = compose_body_with_fm("новый контент\n", CARD);
        assert!(out.starts_with("---\nstatus: In Progress"));
        assert!(out.contains("новый контент"));
        assert_eq!(out.matches("---\n").count(), 2);
    }

    #[test]
    fn без_карточки_нет_пустого_блока() {
        let out = compose_body_with_fm("контент", "плоский старый текст\n");
        assert_eq!(out, "контент", "пустой ---\\n--- не создаётся");
    }

    #[test]
    fn новый_fm_побеждает_старый() {
        let out = compose_body_with_fm("---\nstatus: Todo\n---\n\nx\n", CARD);
        assert!(out.starts_with("---\nstatus: Todo"));
    }

    #[test]
    fn fm_block_границы() {
        assert!(fm_block("нет карточки").is_none());
        assert!(fm_block("---\n---\n").is_some());
        let b = fm_block(CARD).unwrap();
        assert_eq!(b, "---\nstatus: In Progress\nbranch: work/x\n---\n");
    }
}
