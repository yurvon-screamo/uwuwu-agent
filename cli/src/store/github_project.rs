//! Project v2 мутации GitHubStore: Status (single-select) и «Запланировано»
//! (date) на item задачи. Item резолвится по issue id через GraphQL.
//!
//! Механики проверены live-пробой срез A: project item id —
//! viewer.projectV2(number).items.nodes по issue number; поля —
//! updateProjectV2ItemFieldValue (singleSelectOptionId / date).

use super::github_task::GitHubStore;
use super::github_gh::gh_graphql;
use anyhow::{Context, Result};
use serde_json::json;

/// Статус (+ опционально дата «Запланировано») в Project v2.
/// None-статус — только дата. Дата: ISO-строка YYYY-MM-DD ("" — очистка).
pub(super) async fn set_status_and_planned(
    store: &GitHubStore,
    issue_number: &str,
    status: Option<&str>,
    planned: Option<&str>,
) -> Result<()> {
    // Project v2 — канбан владельца (вид); отсутствие не валило запись:
    // frontmatter тела уже несёт Status/Запланировано для CLI/демонов.
    let Some((project_number, project_id)) = project_for_label(store, issue_number).await? else {
        return Ok(());
    };
    let item_id = item_id_of(store, project_number, issue_number)
        .await?
        .context("issue не в проекте — добавление item отдельным шагом (followup)")?;
    if let Some(status) = status {
        let (field_id, option_id) = status_field_ids(store, &project_id, status).await?;
        gh_graphql(
            "mutation($p: ID!, $i: ID!, $f: ID!, $o: String!) {
                updateProjectV2ItemFieldValue(input: {projectId: $p, itemId: $i,
                    fieldId: $f, value: {singleSelectOptionId: $o}}) { clientMutationId }
            }",
            &json!({ "p": project_id, "i": item_id, "f": field_id, "o": option_id }),
        )
        .await?;
    }
    if let Some(planned) = planned {
        let date = normalize_date(planned)?;
        let field_id = planned_field_id(store, &project_id).await?;
        let value = match date.is_empty() {
            true => json!(null),
            false => json!(date),
        };
        gh_graphql(
            "mutation($p: ID!, $i: ID!, $f: ID!, $d: JSON) {
                updateProjectV2ItemFieldValue(input: {projectId: $p, itemId: $i,
                    fieldId: $f, value: {date: $d}}) { clientMutationId }
            }",
            &json!({ "p": project_id, "i": item_id, "f": field_id, "d": value }),
        )
        .await?;
    }
    Ok(())
}

use super::github_project_resolve::{item_id_of, planned_field_id, project_for_label, status_field_ids};

/// «Запланировано» принимает дисплей-форму ядра (YYYY-MM-DD; хвосты до
/// пробела срезаются). Пустое — очистка клетки.
fn normalize_date(planned: &str) -> Result<String> {
    let head = planned.split([' ', 'T']).next().unwrap_or("");
    if head.is_empty() {
        return Ok(String::new());
    }
    chrono::NaiveDate::parse_from_str(head, "%Y-%m-%d")
        .map(|_| head.to_string())
        .map_err(|e| anyhow::anyhow!("«{planned}» не YYYY-MM-DD: {e}"))
}
