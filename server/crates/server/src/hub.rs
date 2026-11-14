//! gh-hub adapter: Projects v2 доска → доменные задачи (чтение) и
//! мутации носителя (Status-поле + fm-зеркало + issue). Инвариант
//! «fm ≡ поля» держит писатель: каждая мутация пишет оба носителя.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use uwuwu_domain::{Priority, Slot, Status, Task};
use uwuwu_usecase::error::{UsecaseError, UsecaseResult};
use uwuwu_usecase::{CommentRef, TaskBoardRepo};

pub struct GitHubHub {
    repo: String,
    token: String,
    http: reqwest::Client,
}

const QUERY: &str = r#"
  query($owner:String!,$name:String!,$board:String!,$after:String){
    repository(owner:$owner, name:$name){
      projectsV2(first:20, query:$board){
        nodes{
          id title
          field(name: "Status"){ ... on ProjectV2SingleSelectField{ id options{ id name } } }
          items(first:100, after:$after){
            pageInfo{ hasNextPage endCursor }
            nodes{
              content{ __typename ...on Issue{ number title body state } }
              fieldValues(first:30){
                nodes{ __typename
                  ...on ProjectV2ItemFieldSingleSelectValue{
                    name field{ ...on ProjectV2FieldCommon{ id name } }
                  }
                }
              }
            }
          }
        }
      }
    }
  }"#;

/// Потолок страниц на доску (100/стр): защита от бесконечного цикла.
const MAX_PAGES: usize = 10;

/// Снимок доски: строки + id для мутаций (project/field/option/item).
#[derive(Default)]
struct BoardMeta {
    project_id: String,
    status_field_id: String,
    /// имя статуса → id опции
    options: HashMap<String, String>,
    /// номер issue → item id
    items: HashMap<u64, String>,
    /// номер → тело issue (fm-пересборка при мутациях)
    bodies: HashMap<u64, String>,
}

impl GitHubHub {
    pub fn new(repo: String, token: String) -> Self {
        Self {
            repo,
            token,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
        }
    }

    async fn gql(&self, query: &str, variables: Value) -> UsecaseResult<Value> {
        let resp = self
            .http
            .post("https://api.github.com/graphql")
            .bearer_auth(&self.token)
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await
            .map_err(|e| UsecaseError::Board(format!("graphql transport: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Board(format!("graphql body: {e}")))?;
        let body: Value = serde_json::from_str(&text)
            .map_err(|e| UsecaseError::Board(format!("graphql не-JSON (HTTP {status}): {e}")))?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "gh api {status}: {}",
                trunc(&text)
            )));
        }
        if let Some(errors) = body.get("errors") {
            return Err(UsecaseError::Board(format!("graphql errors: {errors}")));
        }
        body.get("data")
            .cloned()
            .ok_or_else(|| UsecaseError::Board("graphql: нет data".into()))
    }

    async fn rest(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> UsecaseResult<(reqwest::StatusCode, Value)> {
        let mut req = self
            .http
            .request(method, format!("https://api.github.com{path}"))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json");
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| UsecaseError::Board(format!("rest transport {path}: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Board(format!("rest body {path}: {e}")))?;
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        Ok((status, value))
    }

    /// Полный снимок доски: строки (домен) + меты мутаций.
    async fn snapshot(&self, board: &str) -> UsecaseResult<(Vec<Task>, BoardMeta)> {
        let (owner, name) = self.repo.split_once('/').ok_or_else(|| {
            UsecaseError::Board(format!("repo в форме owner/name: {}", self.repo))
        })?;
        let mut meta = BoardMeta::default();
        let mut rows: Vec<Task> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let data = self
                .gql(
                    QUERY,
                    json!({ "owner": owner, "name": name, "board": board, "after": cursor }),
                )
                .await?;
            let nodes = data["repository"]["projectsV2"]["nodes"]
                .as_array()
                .ok_or_else(|| UsecaseError::Board("projectsV2.nodes не массив".into()))?;
            let project = nodes
                .iter()
                .find(|p| p["title"].as_str() == Some(board))
                .ok_or_else(|| {
                    UsecaseError::Board(format!(
                        "доска «{board}» не найдена среди {} шт",
                        nodes.len()
                    ))
                })?;
            if meta.project_id.is_empty() {
                collect_meta(project, &mut meta);
                if meta.project_id.is_empty() {
                    return Err(UsecaseError::Board(format!(
                        "доска «{board}»: нет project id"
                    )));
                }
            }
            let items = &project["items"];
            if let Some(page) = items["nodes"].as_array() {
                rows.extend(parse_page(page, &mut meta));
            }
            if items["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
                return Ok((rows, meta));
            }
            cursor = items["pageInfo"]["endCursor"].as_str().map(String::from);
        }
        Err(UsecaseError::Board(format!(
            "доска «{board}» превысила {MAX_PAGES} страниц — гард"
        )))
    }

    async fn meta_of(&self, board: &str) -> UsecaseResult<BoardMeta> {
        let (_, meta) = self.snapshot(board).await?;
        Ok(meta)
    }
}

fn collect_meta(project: &Value, meta: &mut BoardMeta) {
    meta.project_id = project["id"].as_str().unwrap_or_default().to_string();
    let field = &project["field"];
    meta.status_field_id = field["id"].as_str().unwrap_or_default().to_string();
    if let Some(options) = field["options"].as_array() {
        for o in options {
            if let (Some(id), Some(name)) = (o["id"].as_str(), o["name"].as_str()) {
                meta.options.insert(name.to_string(), id.to_string());
            }
        }
    }
    if let Some(items) = project["items"]["nodes"].as_array() {
        parse_page(items, meta);
    }
}

fn parse_page(items: &[Value], meta: &mut BoardMeta) -> Vec<Task> {
    items
        .iter()
        .filter_map(|item| {
            let issue = &item["content"];
            if issue["__typename"].as_str()? != "Issue" {
                return None;
            }
            let number = issue["number"].as_u64()?;
            let title = issue["title"].as_str()?.to_string();
            let body = issue["body"].as_str().unwrap_or_default().to_string();
            let status = item_status(item);
            meta.items
                .insert(number, item["id"].as_str().unwrap_or_default().to_string());
            meta.bodies.insert(number, body.clone());
            Some(task_from(number, title, status, &body))
        })
        .collect()
}

/// Сборка доменной задачи из fm-зеркала тела.
pub fn task_from(number: u64, title: String, status_raw: String, body: &str) -> Task {
    let fm = crate::fm::Frontmatter::parse(body);
    let mut task = Task::new(number, title, Status::parse(&status_raw));
    task.priority = Priority::parse(fm.get("priority").unwrap_or_default());
    task.autodone = fm.get("autodone").map(|v| v != "false").unwrap_or(false);
    task.branch = fm.get("branch").filter(|v| !v.is_empty()).map(String::from);
    task.pr = fm.get("pr").filter(|v| !v.is_empty()).map(String::from);
    for slot in [Slot::Design, Slot::Dev, Slot::PlanReview, Slot::Test] {
        if let Some(s) = fm.get(slot.fm_key()).filter(|v| !v.is_empty()) {
            task.claim(slot, s.to_string());
        }
    }
    task
}

fn item_status(item: &Value) -> String {
    item["fieldValues"]["nodes"]
        .as_array()
        .and_then(|vals| {
            vals.iter().find_map(|v| {
                let field = v["field"]["name"].as_str()?;
                (field == "Status").then(|| v["name"].as_str().unwrap_or_default().to_string())
            })
        })
        .unwrap_or_default()
}

fn trunc(s: &str) -> String {
    s.chars().take(200).collect()
}

#[async_trait]
impl TaskBoardRepo for GitHubHub {
    async fn read_board(&self, board: &str) -> UsecaseResult<Vec<Task>> {
        Ok(self.snapshot(board).await?.0)
    }

    async fn set_status(&self, board: &str, number: u64, status: Status) -> UsecaseResult<()> {
        let meta = self.meta_of(board).await?;
        let option = meta.options.get(status.as_str()).ok_or_else(|| {
            UsecaseError::Board(format!("Status-опция «{}» не найдена", status.as_str()))
        })?;
        let item = meta.items.get(&number).ok_or_else(|| {
            UsecaseError::Board(format!("строка #{number} не на доске «{board}»"))
        })?;
        let field = &meta.status_field_id;
        let mutation = r#"
          mutation($p:ID!,$i:ID!,$f:ID!,$o:String!){
            updateProjectV2ItemFieldValue(input:{
              projectId:$p, itemId:$i, fieldId:$f, value:{singleSelectOptionId:$o}
            }){ projectV2Item{ id } }
          }"#;
        self.gql(
            mutation,
            json!({ "p": meta.project_id, "i": item, "f": field, "o": option }),
        )
        .await?;
        self.patch_fm(number, |fields| {
            fields.insert("status".into(), status.as_str().into());
        })
        .await
    }

    async fn set_session(
        &self,
        board: &str,
        number: u64,
        slot: Slot,
        session: &str,
    ) -> UsecaseResult<()> {
        let _ = board;
        self.patch_fm(number, |fields| {
            fields.insert(slot.fm_key().into(), session.into());
        })
        .await
    }

    async fn post_comment(&self, number: u64, body: &str) -> UsecaseResult<()> {
        let (status, _) = self
            .rest(
                reqwest::Method::POST,
                &format!("/repos/{}/issues/{number}/comments", self.repo),
                Some(json!({ "body": body })),
            )
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "comment #{number}: HTTP {status}"
            )));
        }
        Ok(())
    }

    async fn close_issue(&self, number: u64) -> UsecaseResult<()> {
        let (status, body) = self
            .rest(
                reqwest::Method::PATCH,
                &format!("/repos/{}/issues/{number}", self.repo),
                Some(json!({ "state": "closed" })),
            )
            .await?;
        let closed = status.is_success() && body["state"].as_str() == Some("closed");
        if !closed {
            // гард false-success PATCH (кейс #1127): верификация read-back
            return Err(UsecaseError::Board(format!(
                "close #{number}: false-success PATCH (state={})",
                body["state"]
            )));
        }
        Ok(())
    }

    async fn latest_comments(&self, number: u64) -> UsecaseResult<Vec<CommentRef>> {
        let (status, body) = self
            .rest(
                reqwest::Method::GET,
                &format!("/repos/{}/issues/{number}/comments?per_page=5", self.repo),
                None,
            )
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "comments #{number}: HTTP {status}"
            )));
        }
        let mut out: Vec<CommentRef> = body
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|c| {
                        let created =
                            chrono::DateTime::parse_from_rfc3339(c["created_at"].as_str()?).ok()?;
                        Some(CommentRef {
                            id: c["node_id"].as_str()?.to_string(),
                            created_ms: created.timestamp_millis() as u64,
                            body: c["body"].as_str().unwrap_or_default().to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by_key(|c| c.created_ms);
        Ok(out)
    }
}

impl GitHubHub {
    /// fm-пересборка тела: PATCH с сохранением контента дословно.
    async fn patch_fm(
        &self,
        number: u64,
        mutate: impl FnOnce(&mut std::collections::BTreeMap<String, String>),
    ) -> UsecaseResult<()> {
        let (_, body) = self
            .rest(
                reqwest::Method::GET,
                &format!("/repos/{}/issues/{number}", self.repo),
                None,
            )
            .await?;
        let current = body["body"].as_str().unwrap_or_default().to_string();
        let fm = crate::fm::Frontmatter::parse(&current);
        let mut fields = fm.fields;
        mutate(&mut fields);
        let next = crate::fm::Frontmatter::render(&current, &fields);
        let (status, resp) = self
            .rest(
                reqwest::Method::PATCH,
                &format!("/repos/{}/issues/{number}", self.repo),
                Some(json!({ "body": next })),
            )
            .await?;
        let applied = status.is_success() && resp["body"].as_str() == Some(next.as_str());
        if !applied {
            return Err(UsecaseError::Board(format!(
                "patch body #{number}: не применилось (HTTP {status})"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_from_parses_fm() {
        let body = "---\ndev-session: ses_a\nautodone: true\npriority: high\npr: https://x/1\n---\nконтент";
        let t = task_from(7, "т".into(), "In Progress".into(), body);
        assert_eq!(t.status, Status::InProgress);
        assert_eq!(t.session_of(Slot::Dev), Some("ses_a"));
        assert!(t.autodone);
        assert_eq!(t.priority, Priority::High);
        assert_eq!(t.pr.as_deref(), Some("https://x/1"));
    }

    #[test]
    fn task_from_survives_empty_body() {
        let t = task_from(1, "t".into(), String::new(), "");
        assert_eq!(t.status, Status::Empty);
        assert!(!t.autodone);
    }
}
