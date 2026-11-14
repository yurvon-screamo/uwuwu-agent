//! Project v2 мутации GitHubStore: Status (single-select) на item задачи,
//! add-item при создании. Резолверы — github_project_resolve.rs. Клетки
//! «Запланировано» и прочие — глобальные issue fields (github_issue_fields,
//! ADR-098); Status остаётся нативным Pv2-полем (глобального статуса нет).
//!
//! Порядок записи статуса: item отсутствует → add-item → set-field.

use super::github_project_resolve::{add_issue_to_project, project_for_label, status_field_ids};
use super::github_task::GitHubStore;
use super::github_gh::gh_graphql;
use anyhow::{Context, Result};
use serde_json::json;

/// Статус в Project v2. None-статус — no-op (вызовы без статуса не
/// остались: planned переехал в глобальные поля, задача 1087).
/// Project v2 отсутствует (нет канбана) — no-op.
pub(super) async fn set_status(
    store: &GitHubStore,
    issue_number: &str,
    status: Option<&str>,
) -> Result<()> {
    let Some(status) = status else {
        return Ok(());
    };
    let Some((_, project_id)) = project_for_label(store, issue_number).await? else {
        return Ok(());
    };
    // Item отсутствует → добавить (create-путь: issue создан POST'ом и в
    // проекте ещё не состоит; повторный add-item идемпотентен). Item id —
    // из ответа мутации (свежий item в issue.projectItems может появиться
    // с лагом).
    let item_id = add_issue_to_project(store, &project_id, issue_number).await?;
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
        return Err(err)
            .with_context(|| format!("Projects v2: статус «{status}» не записан (лаг платформы)"));
    }
    Ok(())
}

/// «Запланировано» принимает дисплей-форму ядра (YYYY-MM-DD; хвосты до
/// пробела срезаются). Пустое — очистка клетки (None-значение в set_fields).
pub(crate) fn normalize_date(planned: &str) -> Result<String> {
    let head = planned.split([' ', 'T']).next().unwrap_or("");
    if head.is_empty() {
        return Ok(String::new());
    }
    chrono::NaiveDate::parse_from_str(head, "%Y-%m-%d")
        .map(|_| head.to_string())
        .map_err(|e| anyhow::anyhow!("«{planned}» не YYYY-MM-DD: {e}"))
}

#[cfg(test)]
mod normalize_date_tests {
    use super::normalize_date;

    #[test]
    fn дисплей_форма_срезает_хвост() {
        assert_eq!(normalize_date("2026-11-20 15:04").unwrap(), "2026-11-20");
        assert_eq!(
            normalize_date("2026-11-20T15:04:00Z").unwrap(),
            "2026-11-20"
        );
        assert_eq!(normalize_date("2026-11-20").unwrap(), "2026-11-20");
    }

    #[test]
    fn пустое_очистка_мусор_отказ() {
        assert_eq!(normalize_date("").unwrap(), "");
        assert!(normalize_date("завтра").is_err());
    }
}
