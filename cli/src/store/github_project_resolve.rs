//! Project v2 резолверы GitHubStore: issue → проект по proj-label,
//! item id, поля Status/«Запланировано». Живой вызов — github_project.rs.

use super::github_gh::gh_graphql;
use super::github_task::GitHubStore;
use anyhow::{Context, Result};
use serde_json::json;

/// Канбаны «Задачи <proj>» пер-проектные: label issue → номер проекта.
/// Скоуп: один issue — один proj-label, проект однозначен.
pub(super) async fn project_for_label(
    store: &GitHubStore,
    issue_number: &str,
) -> Result<Option<(i64, String)>> {
    let issue = gh_graphql(
        "query($repo: String!, $owner: String!, $n: Int!) {
            repository(name: $repo, owner: $owner) {
                issue(number: $n) { id labels(first: 10) { nodes { name } } }
            }
        }",
        &(issue_ctx(store, issue_number).await),
    )
    .await?;
    let labels: Vec<String> = issue
        .pointer("/repository/issue/labels/nodes")
        .and_then(|l| l.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|n| n["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let Some(project) = labels.iter().find_map(|l| l.strip_prefix("proj:")) else {
        return Ok(None);
    };
    let number = match project {
        "uwuwu" => Some(5),
        _ => project_number_by_title(store, project).await?,
    };
    let Some(number) = number else {
        return Ok(None);
    };
    let id = project_id_by_number(store, number).await?;
    Ok(Some((number, id)))
}

async fn project_number_by_title(_store: &GitHubStore, project: &str) -> Result<Option<i64>> {
    let data = gh_graphql(
        "query { viewer { projectsV2(first: 30) { nodes {
            number title } } } }",
        &json!({}),
    )
    .await?;
    let needle = format!("Задачи {project}");
    Ok(data
        .pointer("/viewer/projectsV2/nodes")
        .and_then(|n| n.as_array())
        .and_then(|a| {
            a.iter()
                .find(|p| p["title"].as_str() == Some(needle.as_str()))
                .and_then(|p| p["number"].as_i64())
        }))
}

async fn project_id_by_number(_store: &GitHubStore, number: i64) -> Result<String> {
    let data = gh_graphql(
        "query($n: Int!) { viewer { projectV2(number: $n) { id } } }",
        &json!({ "n": number }),
    )
    .await?;
    data.pointer("/viewer/projectV2/id")
        .and_then(|i| i.as_str())
        .map(String::from)
        .context("Project v2 id пуст")
}

async fn issue_ctx(store: &GitHubStore, issue_number: &str) -> serde_json::Value {
    let (owner, repo) = store
        .repo
        .split_once('/')
        .unwrap_or(("", store.repo.as_str()));
    json!({
        "repo": repo,
        "owner": owner,
        "n": issue_number.parse::<i64>().unwrap_or(0),
    })
}

pub(super) async fn item_id_of(
    _store: &GitHubStore,
    project_number: i64,
    issue_number: &str,
) -> Result<Option<String>> {
    let n = issue_number.parse::<i64>()?;
    let data = gh_graphql(
        "query($n: Int!) { viewer { projectV2(number: $n) {
            items(first: 100) { nodes { id content {
                ... on Issue { number } } } } } } }",
        &json!({ "n": project_number }),
    )
    .await?;
    Ok(data
        .pointer("/viewer/projectV2/items/nodes")
        .and_then(|nodes| nodes.as_array())
        .and_then(|a| {
            a.iter()
                .find(|item| item.pointer("/content/number").and_then(|x| x.as_i64()) == Some(n))
        })
        .and_then(|item| item["id"].as_str())
        .map(String::from))
}

pub(super) async fn status_field_ids(
    _store: &GitHubStore,
    project_id: &str,
    status: &str,
) -> Result<(String, String)> {
    let data = gh_graphql(
        "query($id: ID!) { node(id: $id) { ... on ProjectV2 {
            field(name: \"Status\") { ... on ProjectV2SingleSelectField {
                id options { id name } } } } } }",
        &json!({ "id": project_id }),
    )
    .await?;
    let field_id = data
        .pointer("/node/field/id")
        .and_then(|i| i.as_str())
        .context("Status-поле проекта не найдено")?
        .to_string();
    let needle = status.to_string();
    let option_id = data
        .pointer("/node/field/options")
        .and_then(|o| o.as_array())
        .and_then(|a| {
            a.iter()
                .find(|opt| opt["name"].as_str() == Some(needle.as_str()))
                .and_then(|opt| opt["id"].as_str())
        })
        .with_context(|| format!("опция Status «{needle}» не найдена (канон срез A)"))?
        .to_string();
    Ok((field_id, option_id))
}

pub(super) async fn planned_field_id(_store: &GitHubStore, project_id: &str) -> Result<String> {
    let data = gh_graphql(
        "query($id: ID!) { node(id: $id) { ... on ProjectV2 {
            field(name: \"Запланировано\") { ... on ProjectV2Field { id } } } } }",
        &json!({ "id": project_id }),
    )
    .await?;
    data.pointer("/node/field/id")
        .and_then(|i| i.as_str())
        .map(String::from)
        .context("поле «Запланировано» не найдено (срез A)")
}
