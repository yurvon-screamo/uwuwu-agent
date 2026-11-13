//! Project v2 мутации GitHubStore: Status (single-select) и «Запланировано»
//! (date) на item задачи, add-item при создании. Резолверы —
//! github_project_resolve.rs.
//!
//! Порядок записи статуса: item отсутствует → add-item → set-field. Frontmatter
//! тела уже несёт статус (github_task_write) — канбан-запись синхронизирует
//! Projects v2 (вид владельца).

use super::github_project_resolve::{add_issue_to_project, project_for_label, status_field_ids};
use super::github_task::GitHubStore;
use super::github_gh::gh_graphql;
use anyhow::{Context, Result};
use serde_json::json;

/// Статус (+ опционально дата «Запланировано») в Project v2. None-статус —
/// только дата. Дата: ISO-строка YYYY-MM-DD ("" — очистка).
/// Project v2 отсутствует (нет канбана) — no-op: frontmatter тела несёт
/// статус для CLI/демонов.
pub(super) async fn set_status_and_planned(
    store: &GitHubStore,
    issue_number: &str,
    status: Option<&str>,
    planned: Option<&str>,
) -> Result<()> {
    let Some((_, project_id)) = project_for_label(store, issue_number).await? else {
        return Ok(());
    };
    // Item отсутствует → добавить (create-путь: issue создан POST'ом и в
    // проекте ещё не состоит; повторный add-item идемпотентен). Item id —
    // из ответа мутации (свежий item в issue.projectItems может появиться
    // с лагом).
    let item_id = add_issue_to_project(store, &project_id, issue_number).await?;
    if let Some(status) = status {
        let (field_id, option_id) = status_field_ids(&project_id, status).await?;
        let mut last_err: Option<anyhow::Error> = None;
        for delay in [0u64, 2, 4, 8] {
            if delay > 0 {
                // Свежедобавленный item виден для update с лагом (проба
                // 13.11: add-ok, update NOT_FOUND до ~4s) — ретраи с бэкоффом
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            }
            match gh_graphql(
                "mutation($p: ID!, $i: ID!, $f: ID!, $o: String!) {
                    updateProjectV2ItemFieldValue(input: {projectId: $p, itemId: $i,
                        fieldId: $f, value: {singleSelectOptionId: $o}}) { clientMutationId }
                }",
                &json!({ "p": project_id, "i": item_id, "f": field_id, "o": option_id }),
            )
            .await
            {
                Ok(_) => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    let text = format!("{e:#}");
                    if text.contains("Could not resolve to a node") {
                        last_err = Some(anyhow::anyhow!(text));
                        continue;
                    }
                    return Err(e)
                        .with_context(|| format!("Projects v2: статус «{status}» не записан"));
                }
            }
        }
        if let Some(err) = last_err {
            return Err(err).with_context(|| {
                format!("Projects v2: статус «{status}» не записан (лаг платформы)")
            });
        }
    }
    if let Some(planned) = planned {
        // «Запланировано» — глобальный issue field (REST), не project-поле:
        // путешествует с issue вне канбанов, дубли project-полей снесены
        let date = normalize_date(planned)?;
        set_global_field_value(store, issue_number, "Запланировано", &date).await?;
    }
    Ok(())
}

/// Значение глобального issue field (text/date) по имени поля. Дата —
/// ISO YYYY-MM-DD; пустая строка значение не сбрасывает (skip).
pub(crate) async fn set_global_field_value(
    store: &GitHubStore,
    issue_number: &str,
    field_name: &str,
    value: &str,
) -> Result<()> {
    if value.trim().is_empty() {
        return Ok(());
    }
    let fields = super::github_gh::gh_json(&["api", "/user/issue-fields"], None).await?;
    let fid = fields
        .as_array()
        .and_then(|arr| {
            arr.iter()
                .find(|f| f.get("name").and_then(|n| n.as_str()) == Some(field_name))
                .and_then(|f| f.get("id").and_then(|i| i.as_i64()))
        })
        .with_context(|| format!("глобальное поле «{field_name}» не найдено"))?;
    super::github_gh::gh_json(
        &[
            "api",
            "-X",
            "POST",
            &format!(
                "repos/{}/issues/{issue_number}/issue-field-values",
                store.repo
            ),
        ],
        Some(
            &json!({
                "issue_field_values": [ { "field_id": fid, "value": value } ]
            })
            .to_string(),
        ),
    )
    .await
    .with_context(|| format!("issue field «{field_name}» не записан"))?;
    Ok(())
}

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
