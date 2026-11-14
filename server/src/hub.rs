//! gh-hub adapter: чтение доски проекта (Projects v2 → строки задач).
//! GraphQL с пагинацией; доска матчится по title точно (query: фильтр
//! GitHub неточный — возвращает соседние доски).

use crate::domain::TaskRow;
use async_trait::async_trait;
use serde_json::{json, Value};

/// Трейт юзкейс-слоя: инфра реализует поверх GitHub GraphQL.
#[async_trait]
pub trait BoardReader: Send + Sync {
    /// Читает строки доски по названию.
    async fn read_board(&self, board: &str) -> anyhow::Result<Vec<TaskRow>>;
}

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
          title
          items(first:100, after:$after){
            pageInfo{ hasNextPage endCursor }
            nodes{
              content{ __typename ...on Issue{ number title body state } }
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

/// Потолок страниц на доску (100/стр): защита от бесконечного цикла.
const MAX_PAGES: usize = 10;

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

    async fn gql(&self, variables: Value) -> anyhow::Result<Value> {
        let resp = self
            .http
            .post("https://api.github.com/graphql")
            .bearer_auth(&self.token)
            .json(&json!({ "query": QUERY, "variables": variables }))
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        let body: Value = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("gh graphql не-JSON (HTTP {status}): {e}"))?;
        if !status.is_success() {
            anyhow::bail!("gh api {status}: {}", truncate(&text));
        }
        if let Some(errors) = body.get("errors") {
            anyhow::bail!("graphql errors: {errors}");
        }
        body.get("data")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("graphql: нет data"))
    }

    pub async fn snapshot(&self, board: &str) -> anyhow::Result<Vec<TaskRow>> {
        let (owner, name) = self
            .repo
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("repo в форме owner/name: {}", self.repo))?;
        let mut cursor: Option<String> = None;
        let mut rows: Vec<TaskRow> = Vec::new();
        for _ in 0..MAX_PAGES {
            let data = self
                .gql(json!({
                    "owner": owner, "name": name, "board": board,
                    "after": cursor,
                }))
                .await?;
            let nodes = data["repository"]["projectsV2"]["nodes"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("projectsV2.nodes не массив"))?;
            let project = nodes
                .iter()
                .find(|p| p["title"].as_str() == Some(board))
                .ok_or_else(|| {
                    anyhow::anyhow!("доска «{board}» не найдена среди {} шт", nodes.len())
                })?;
            let items = &project["items"];
            if let Some(page) = items["nodes"].as_array() {
                rows.extend(parse_items(page));
            }
            let page_info = &items["pageInfo"];
            if page_info["hasNextPage"].as_bool() != Some(true) {
                return Ok(rows);
            }
            cursor = page_info["endCursor"].as_str().map(String::from);
        }
        Err(anyhow::anyhow!(
            "доска «{board}» превысила {} страниц — гард",
            MAX_PAGES
        ))
    }
}

fn parse_items(items: &[Value]) -> Vec<TaskRow> {
    items
        .iter()
        .filter_map(|item| {
            let issue = &item["content"];
            if issue["__typename"].as_str()? != "Issue" {
                return None;
            }
            let number = issue["number"].as_u64()?;
            let title = issue["title"].as_str()?.to_string();
            let body = issue["body"].as_str().unwrap_or_default();
            let status = item_status(item);
            Some(TaskRow::from_parts(number, title, &status, body))
        })
        .collect()
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

fn truncate(s: &str) -> String {
    s.chars().take(200).collect()
}

#[async_trait]
impl BoardReader for GitHubHub {
    async fn read_board(&self, board: &str) -> anyhow::Result<Vec<TaskRow>> {
        self.snapshot(board).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(number: u64, status: &str, body: &str, typename: &str) -> Value {
        json!({
            "content": if typename == "Issue" {
                json!({ "__typename": "Issue", "number": number, "title": format!("t{number}"),
                        "body": body, "state": "open" })
            } else {
                json!({ "__typename": typename })
            },
            "fieldValues": { "nodes": [
                { "__typename": "ProjectV2ItemFieldSingleSelectValue",
                  "name": status, "field": { "name": "Status" } },
                { "__typename": "ProjectV2ItemFieldTextValue",
                  "text": "x", "field": { "name": "Title" } }
            ]}
        })
    }

    #[test]
    fn parses_items_with_status_and_fm() {
        let body = "---\ndev-session: ses_a\nautodone: true\n---\nконтент";
        let items = vec![
            item(1, "In Progress", body, "Issue"),
            item(2, "Draft", "---\n---\nчерновик", "Issue"),
            item(3, "", "", "PullRequest"),
        ];
        let rows = parse_items(&items);
        assert_eq!(rows.len(), 2, "PR-контент отфильтрован");
        assert_eq!(rows[0].number, 1);
        assert_eq!(rows[0].status, crate::domain::Status::InProgress);
        assert_eq!(rows[0].session_of(crate::domain::Slot::Dev), Some("ses_a"));
        assert!(rows[0].autodone);
        assert_eq!(rows[1].status, crate::domain::Status::Draft);
    }

    #[test]
    fn missing_status_is_empty() {
        let items = vec![json!({ "content": {
            "__typename": "Issue", "number": 9, "title": "t", "body": "", "state": "open"
        }})];
        let rows = parse_items(&items);
        assert_eq!(rows[0].status, crate::domain::Status::Empty);
    }
}
