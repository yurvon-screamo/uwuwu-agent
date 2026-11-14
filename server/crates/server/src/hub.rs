//! gh-hub adapter: доска (bulk select-колонки) + issue REST (тело/комменты/
//! нативные поля). fm-зеркало снесено (владелец 15.11): операционные данные —
//! Status/Приоритет/Autodone на доске, сессии/ветка/PR — глобальные поля issue
//! (fields.rs). Тело задачи — чистый контент.

use crate::fields::{parse_field_values, Fields, IssueFields};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use uwuwu_domain::{Priority, Status, Task};
use uwuwu_usecase::error::{UsecaseError, UsecaseResult};
use uwuwu_usecase::{CommentRef, IssueView, TaskBoardRepo};

pub struct GitHubHub {
    repo: String,
    token: String,
    http: reqwest::Client,
    /// Нативные поля issue (сессии/ветка/PR/тип).
    pub fields: Arc<IssueFields>,
}

const QUERY: &str = r#"
  query($owner:String!,$name:String!,$board:String!,$after:String){
    repository(owner:$owner, name:$name){
      projectsV2(first:20, query:$board){
        nodes{
          id title
          field(name:"Status"){ ...on ProjectV2SingleSelectField{ id options{ id name } } }
          items(first:100, after:$after){
            pageInfo{ hasNextPage endCursor }
            nodes{
              id
              content{ __typename ...on Issue{ number title state } }
              fieldValues(first:30){
                nodes{ __typename
                  ...on ProjectV2ItemFieldSingleSelectValue{
                    name field{ ...on ProjectV2FieldCommon{ name } }
                  }
                }
              }
            }
          }
        }
      }
    }
  }"#;

const MAX_PAGES: usize = 10;

#[derive(Default)]
struct BoardMeta {
    project_id: String,
    status_field_id: String,
    options: std::collections::HashMap<String, String>,
    items: std::collections::HashMap<u64, String>,
}

impl GitHubHub {
    pub fn new(repo: String, token: String) -> Self {
        Self {
            fields: Arc::new(IssueFields::new(repo.clone(), token.clone())),
            repo,
            token,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
        }
    }

    pub fn fields(&self) -> Arc<IssueFields> {
        self.fields.clone()
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

    /// Снимок доски: bulk-задачи (Status/Приоритет/Autodone) + мета мутаций.
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
                meta.project_id = project["id"].as_str().unwrap_or_default().to_string();
                meta.status_field_id = project["field"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if let Some(options) = project["field"]["options"].as_array() {
                    for o in options {
                        if let (Some(id), Some(n)) = (o["id"].as_str(), o["name"].as_str()) {
                            meta.options.insert(n.to_string(), id.to_string());
                        }
                    }
                }
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

    /// Статус строки на доске (по номеру issue, через projectItems).
    async fn board_status(&self, number: u64) -> UsecaseResult<String> {
        let q = r#"
          query($owner:String!,$name:String!,$n:Int!){
            repository(owner:$owner,name:$name){
              issue(number:$n){
                projectItems(first:3){
                  nodes{ fieldValues(first:20){ nodes{
                    __typename ...on ProjectV2ItemFieldSingleSelectValue{
                      name field{ ...on ProjectV2FieldCommon{ name } } } } } }
                }
              }
            }
          }"#;
        let (owner, name) = self
            .repo
            .split_once('/')
            .ok_or_else(|| UsecaseError::Board(format!("repo: {}", self.repo)))?;
        let data = self
            .gql(
                q,
                json!({ "owner": owner, "name": name, "n": number as i64 }),
            )
            .await?;
        if let Some(items) = data["repository"]["issue"]["projectItems"]["nodes"].as_array() {
            for item in items {
                if let Some(vals) = item["fieldValues"]["nodes"].as_array() {
                    for v in vals {
                        if v["field"]["name"].as_str() == Some("Status") {
                            return Ok(v["name"].as_str().unwrap_or_default().to_string());
                        }
                    }
                }
            }
        }
        Ok(String::new())
    }

    /// Комменты задачи: последняя страница (свежие), по возрастанию.
    async fn all_comments(&self, number: u64) -> UsecaseResult<Vec<CommentRef>> {
        let (status, body) = self
            .rest(
                reqwest::Method::GET,
                &format!("/repos/{}/issues/{number}/comments?per_page=100", self.repo),
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
            let v = item_values(item);
            let mut task = Task::new(number, title, Status::parse(&v.status));
            task.priority = Priority::parse(&v.priority);
            task.autodone = v.autodone;
            if issue["state"].as_str() == Some("closed") {
                task.status = Status::Done;
            }
            meta.items
                .insert(number, item["id"].as_str().unwrap_or_default().to_string());
            Some(task)
        })
        .collect()
}

/// Bulk-значения select-колонок доски (поля issue ленивые — fields.rs).
struct ItemValues {
    status: String,
    priority: String,
    autodone: bool,
}

fn item_values(item: &Value) -> ItemValues {
    let mut out = ItemValues {
        status: String::new(),
        priority: String::new(),
        autodone: false,
    };
    if let Some(vals) = item["fieldValues"]["nodes"].as_array() {
        for v in vals {
            let field = v["field"]["name"].as_str().unwrap_or_default();
            let value = v["name"].as_str().unwrap_or_default();
            match field {
                "Status" => out.status = value.to_string(),
                "Приоритет" | "Priority" => out.priority = value.to_string(),
                "Autodone" => out.autodone = value == "on" || value == "true",
                _ => {}
            }
        }
    }
    out
}

fn trunc(s: &str) -> String {
    s.chars().take(200).collect()
}

/// Маппинг сырых имён полей → типизированный TaskFields (домен не знает имён).
fn map_task_fields(raw: &Fields) -> uwuwu_domain::TaskFields {
    uwuwu_domain::TaskFields {
        dev_session: raw.get(crate::fields::F_DEV_SESSION).cloned(),
        review_session: raw.get(crate::fields::F_REVIEW_SESSION).cloned(),
        test_session: raw.get(crate::fields::F_TEST_SESSION).cloned(),
        branch: raw.get("Branch").cloned(),
        pr: raw.get("PR").cloned(),
    }
}

#[async_trait]
impl TaskBoardRepo for GitHubHub {
    async fn read_board(&self, board: &str) -> UsecaseResult<Vec<Task>> {
        Ok(self.snapshot(board).await?.0)
    }

    async fn set_status(&self, board: &str, number: u64, status: Status) -> UsecaseResult<()> {
        let (_, mut meta) = self.snapshot(board).await?;
        let option = meta.options.remove(status.as_str()).ok_or_else(|| {
            UsecaseError::Board(format!("Status-опция «{}» не найдена", status.as_str()))
        })?;
        let item = meta.items.remove(&number).ok_or_else(|| {
            UsecaseError::Board(format!("строка #{number} не на доске «{board}»"))
        })?;
        let mutation = r#"
          mutation($p:ID!,$i:ID!,$f:ID!,$o:String!){
            updateProjectV2ItemFieldValue(input:{
              projectId:$p, itemId:$i, fieldId:$f, value:{singleSelectOptionId:$o}
            }){ projectV2Item{ id } }
          }"#;
        self.gql(
            mutation,
            json!({ "p": meta.project_id, "i": item, "f": meta.status_field_id, "o": option }),
        )
        .await?;
        Ok(())
    }

    async fn set_session(
        &self,
        _board: &str,
        number: u64,
        slot: uwuwu_domain::Slot,
        session: &str,
    ) -> UsecaseResult<()> {
        let name = match slot {
            uwuwu_domain::Slot::Dev => crate::fields::F_DEV_SESSION,
            uwuwu_domain::Slot::PlanReview | uwuwu_domain::Slot::ResultReview => {
                crate::fields::F_REVIEW_SESSION
            }
            uwuwu_domain::Slot::Test => crate::fields::F_TEST_SESSION,
            uwuwu_domain::Slot::Design => crate::fields::F_DEV_SESSION,
        };
        self.fields
            .store(number, &[(name, Some(session.to_string()))])
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
        self.close_issue_ext(number, false).await
    }

    async fn read_issue(&self, _board: &str, number: u64) -> UsecaseResult<IssueView> {
        let (_, issue) = self
            .rest(
                reqwest::Method::GET,
                &format!("/repos/{}/issues/{number}", self.repo),
                None,
            )
            .await?;
        let title = issue["title"].as_str().unwrap_or_default().to_string();
        let body = issue["body"].as_str().unwrap_or_default().to_string();
        let status = self.board_status(number).await.unwrap_or_default();
        let mut task = Task::new(number, title, Status::parse(&status));
        task.apply_fields(&map_task_fields(&parse_field_values(&issue)));
        if issue["state"].as_str() == Some("closed") {
            task.status = Status::Done;
        }
        let comments = self.all_comments(number).await?;
        Ok(IssueView {
            task,
            body,
            comments,
        })
    }

    async fn create_issue(
        &self,
        board: &str,
        project: &str,
        title: &str,
        body: &str,
    ) -> UsecaseResult<u64> {
        let (status, resp) = self
            .rest(
                reqwest::Method::POST,
                &format!("/repos/{}/issues", self.repo),
                Some(
                    json!({ "title": title, "body": body, "labels": [format!("proj:{project}")] }),
                ),
            )
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "create issue: HTTP {status}: {}",
                resp.to_string().chars().take(140).collect::<String>()
            )));
        }
        let number = resp["number"].as_u64().unwrap_or_default();
        if number == 0 {
            return Err(UsecaseError::Board("create issue: нет номера".into()));
        }
        let node_id = resp["node_id"].as_str().unwrap_or_default().to_string();
        self.add_to_board(board, number, &node_id).await?;
        Ok(number)
    }

    async fn rename_issue(&self, number: u64, title: &str) -> UsecaseResult<()> {
        let (status, _) = self
            .rest(
                reqwest::Method::PATCH,
                &format!("/repos/{}/issues/{number}", self.repo),
                Some(json!({ "title": title })),
            )
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "rename #{number}: HTTP {status}"
            )));
        }
        Ok(())
    }

    async fn patch_cells(
        &self,
        _number: u64,
        _mutate: uwuwu_usecase::FmMutator,
    ) -> UsecaseResult<()> {
        // fm-ячейки снесены: ветка/PR/приоритет — нативные поля (taskwrite)
        Ok(())
    }

    async fn close_issue_ext(&self, number: u64, not_planned: bool) -> UsecaseResult<()> {
        let reason = if not_planned {
            "not_planned"
        } else {
            "completed"
        };
        let (status, body) = self
            .rest(
                reqwest::Method::PATCH,
                &format!("/repos/{}/issues/{number}", self.repo),
                Some(json!({ "state": "closed", "state_reason": reason })),
            )
            .await?;
        let closed = status.is_success() && body["state"].as_str() == Some("closed");
        if !closed {
            return Err(UsecaseError::Board(format!(
                "close #{number}: false-success PATCH (state={})",
                body["state"]
            )));
        }
        Ok(())
    }

    async fn replace_body(&self, number: u64, body_md: &str) -> UsecaseResult<()> {
        let (status, resp) = self
            .rest(
                reqwest::Method::PATCH,
                &format!("/repos/{}/issues/{number}", self.repo),
                Some(json!({ "body": body_md })),
            )
            .await?;
        let applied = status.is_success() && resp["body"].as_str() == Some(body_md);
        if !applied {
            return Err(UsecaseError::Board(format!(
                "replace body #{number}: не применилось (HTTP {status})"
            )));
        }
        Ok(())
    }

    async fn latest_comments(&self, number: u64) -> UsecaseResult<Vec<CommentRef>> {
        self.all_comments(number).await
    }
}

impl GitHubHub {
    /// Поля задачи (нативные): лениво для строк-кандидатов тика.
    #[allow(dead_code)] // точка входа ленивых чтений тика (срез анти-шторма подключит)
    pub async fn load_fields(&self, number: u64) -> UsecaseResult<Fields> {
        self.fields.load(number).await
    }

    async fn add_to_board(&self, board: &str, number: u64, node_id: &str) -> UsecaseResult<()> {
        let (_, meta) = self.snapshot(board).await?;
        let mutation = r#"
          mutation($p:ID!,$c:ID!){ addProjectV2ItemById(input:{projectId:$p, contentId:$c}){
            item{ id } } }"#;
        let added = self
            .gql(mutation, json!({ "p": meta.project_id, "c": node_id }))
            .await?;
        let item = added["addProjectV2ItemById"]["item"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if item.is_empty() {
            return Err(UsecaseError::Board(format!("add #{number}: нет item id")));
        }
        if let Some(draft) = meta.options.get("Draft") {
            let set = r#"
              mutation($p:ID!,$i:ID!,$f:ID!,$o:String!){
                updateProjectV2ItemFieldValue(input:{projectId:$p,itemId:$i,fieldId:$f,
                  value:{singleSelectOptionId:$o}}){ projectV2Item{ id } } }"#;
            self.gql(
                set,
                json!({ "p": meta.project_id, "i": item, "f": meta.status_field_id, "o": draft }),
            )
            .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_values_maps_board_columns() {
        let item = json!({
            "content": { "__typename": "Issue", "number": 1, "title": "t", "state": "open" },
            "fieldValues": { "nodes": [
                { "name": "In Progress", "field": { "name": "Status" } },
                { "name": "high", "field": { "name": "Приоритет" } },
                { "name": "on", "field": { "name": "Autodone" } },
            ]}
        });
        let v = item_values(&item);
        assert_eq!(v.status, "In Progress");
        assert_eq!(v.priority, "high");
        assert!(v.autodone);
    }

    #[test]
    fn parse_page_marks_closed_done() {
        let mut meta = BoardMeta::default();
        let items = vec![json!({
            "id": "IT_x",
            "content": { "__typename": "Issue", "number": 7, "title": "t", "state": "closed" },
            "fieldValues": { "nodes": [] }
        })];
        let rows = parse_page(&items, &mut meta);
        assert_eq!(rows[0].status, Status::Done);
        assert_eq!(meta.items.get(&7).map(String::as_str), Some("IT_x"));
    }
}
