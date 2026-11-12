//! Project v2 резолверы и мутации GitHubStore: issue → проект по proj-label,
//! item-резолв (через issue.projectItems — без пагинации project'а),
//! add-item, поля Status/«Запланировано». Живое покрытие — e2e-риг
//! github_store_e2e (create на доске с канбаном).

use super::github_gh::gh_graphql;
use super::github_task::GitHubStore;
use anyhow::{Context, Result};
use serde_json::json;

/// Канбаны «Задачи <proj>» пер-проектные: label issue → (номер, id) проекта.
/// None — у issue нет proj-label (status-поле не применимо).
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
    let Some(number) = project_number_by_title(project).await? else {
        return Ok(None);
    };
    let id = project_id_by_number(number).await?;
    Ok(Some((number, id)))
}

/// Пагинация не нужна: viewer.projectsV2 — собственные проекты (десятки,
/// first: 30 с запасом); список короткий и полный.
async fn project_number_by_title(project: &str) -> Result<Option<i64>> {
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

async fn project_id_by_number(number: i64) -> Result<String> {
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

/// Node-id issue (для addProjectV2ItemById contentId).
pub(super) async fn issue_node_id(store: &GitHubStore, issue_number: &str) -> Result<String> {
    let data = gh_graphql(
        "query($repo: String!, $owner: String!, $n: Int!) {
            repository(name: $repo, owner: $owner) {
                issue(number: $n) { id }
            }
        }",
        &(issue_ctx(store, issue_number).await),
    )
    .await?;
    data.pointer("/repository/issue/id")
        .and_then(|i| i.as_str())
        .map(String::from)
        .context("issue node id пуст")
}

/// Item id через issue.projectItems (пагинация project items не нужна:
/// issue входит в 1–2 проекта, first: 10 покрывает).
pub(super) async fn item_id_of(
    store: &GitHubStore,
    issue_number: &str,
) -> Result<Option<(String, String)>> {
    let n = issue_number.parse::<i64>()?;
    let data = gh_graphql(
        "query($repo: String!, $owner: String!, $n: Int!) {
            repository(name: $repo, owner: $owner) {
                issue(number: $n) {
                    projectItems(first: 10) {
                        nodes {
                            id
                            project { id ... on ProjectV2 { number } }
                        }
                    }
                }
            }
        }",
        &(issue_ctx(store, issue_number).await),
    )
    .await?;
    Ok(data
        .pointer("/repository/issue/projectItems/nodes")
        .and_then(|nodes| nodes.as_array())
        .and_then(|a| {
            a.iter()
                .find(|item| item.pointer("/project/number").and_then(|x| x.as_i64()) == Some(n))
                .and_then(|item| {
                    let id = item["id"].as_str()?.to_string();
                    let project_id = item
                        .pointer("/project/id")
                        .and_then(|x| x.as_str())?
                        .to_string();
                    Some((id, project_id))
                })
        }))
}

/// Добавить issue в Project v2 (addProjectV2ItemById, contentId = issue
/// node id). Идемпотентен: уже в проекте — no-op.
pub(super) async fn add_issue_to_project(
    store: &GitHubStore,
    project_id: &str,
    issue_number: &str,
) -> Result<()> {
    if item_in_project(store, project_id, issue_number).await? {
        return Ok(());
    }
    let content_id = issue_node_id(store, issue_number).await?;
    gh_graphql(
        "mutation($p: ID!, $c: ID!) {
            addProjectV2ItemById(input: {projectId: $p, contentId: $c}) {
                item { id }
            }
        }",
        &json!({ "p": project_id, "c": content_id }),
    )
    .await?;
    Ok(())
}

/// Item issue в проекте: issue.projectItems — прямая связь, пагинация
/// project-стороны не нужна.
pub(super) async fn item_in_project(
    store: &GitHubStore,
    project_id: &str,
    issue_number: &str,
) -> Result<bool> {
    let data = gh_graphql(
        "query($repo: String!, $owner: String!, $n: Int!) {
            repository(name: $repo, owner: $owner) {
                issue(number: $n) {
                    projectItems(first: 10) {
                        nodes { project { id } }
                    }
                }
            }
        }",
        &(issue_ctx(store, issue_number).await),
    )
    .await?;
    Ok(data
        .pointer("/repository/issue/projectItems/nodes")
        .and_then(|nodes| nodes.as_array())
        .map(|a| {
            a.iter().any(|item| {
                item.pointer("/project/id").and_then(|x| x.as_str()) == Some(project_id)
            })
        })
        .unwrap_or(false))
}

pub(super) async fn status_field_ids(project_id: &str, status: &str) -> Result<(String, String)> {
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

pub(super) async fn planned_field_id(project_id: &str) -> Result<String> {
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
