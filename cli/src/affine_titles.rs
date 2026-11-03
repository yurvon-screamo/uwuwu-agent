//! Batch doc-title resolution via the AFFiNE UI GraphQL API.
//!
//! One aliased query resolves any number of titles in a single POST —
//! kanban rows carry their title on the linked page, so `task list/get`
//! would otherwise do one heavy `get_doc` per row through the MCP gateway
//! (the N+1 this module removes). A missing doc comes back as
//! `{id, title: null}` without GraphQL errors — mapped to `None`, the
//! caller renders a placeholder. UI credentials are required; without
//! them title-substring matching is unavailable (honest "use the id").

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::affine_gql::AffineUi;

/// Doc ids are AFFiNE NanoID-like tokens; anything else would break the
/// aliased query or smuggle GraphQL syntax.
fn validate_doc_id(id: &str) -> Result<()> {
    if !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Ok(())
    } else {
        Err(anyhow::anyhow!("некорректный docId: {id:?}"))
    }
}

/// Full request body: `d0..dN` aliases over `doc(docId: $v0..$vN)`.
fn build_titles_request(workspace: &str, doc_ids: &[String]) -> Result<Value> {
    if doc_ids.is_empty() {
        anyhow::bail!("пустой список docId");
    }
    for id in doc_ids {
        validate_doc_id(id)?;
    }
    let fields: Vec<String> = doc_ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("d{i}: doc(docId: $v{i}) {{ id title }}"))
        .collect();
    let var_defs: Vec<String> = (0..doc_ids.len())
        .map(|i| format!("$v{i}: String!"))
        .collect();
    let mut variables = json!({ "w": workspace });
    for (i, id) in doc_ids.iter().enumerate() {
        variables[format!("v{i}")] = json!(id);
    }
    Ok(json!({
        "query": format!(
            "query($w: String!, {}) {{ workspace(id: $w) {{ {} }} }}",
            var_defs.join(", "),
            fields.join(" ")
        ),
        "variables": variables,
    }))
}

/// Map the payload back to docId → title. Missing/broken docs are `None`.
/// Workspace-level failures (auth, unknown workspace) are errors.
fn parse_titles_payload(
    payload: &Value,
    doc_ids: &[String],
) -> Result<HashMap<String, Option<String>>> {
    if let Some(errs) = payload.get("errors").and_then(|e| e.as_array()) {
        let msg = errs
            .first()
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("unknown graphql error");
        anyhow::bail!("affine titles: {msg}");
    }
    let workspace = payload
        .pointer("/data/workspace")
        .ok_or_else(|| anyhow::anyhow!("affine titles: ответ без data.workspace"))?;
    if !workspace.is_object() {
        anyhow::bail!("affine titles: workspace null — проверь UI-креды и воркспейс");
    }
    let mut out = HashMap::with_capacity(doc_ids.len());
    for (i, id) in doc_ids.iter().enumerate() {
        let title = workspace
            .get(format!("d{i}"))
            .and_then(|d| d.get("title"))
            .and_then(|t| t.as_str())
            .map(String::from);
        out.insert(id.clone(), title);
    }
    Ok(out)
}

/// One POST for all ids; missing docs → `None` in the map.
pub(crate) async fn fetch_titles(
    ui: &AffineUi,
    workspace: &str,
    doc_ids: &[String],
) -> Result<HashMap<String, Option<String>>> {
    let request = build_titles_request(workspace, doc_ids)?;
    let payload = ui
        .query(&request)
        .await
        .context("batch doc titles request")?;
    parse_titles_payload(&payload, doc_ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_request_uses_aliases_and_variables() {
        let ids = vec!["AAA111".to_string(), "bbb-222".to_string()];

        let body = build_titles_request("ws123", &ids).expect("valid ids");

        let query = body["query"].as_str().expect("query string");
        assert!(query.contains("d0: doc(docId: $v0) { id title }"));
        assert!(query.contains("d1: doc(docId: $v1) { id title }"));
        assert!(query.contains("$v0: String!, $v1: String!"));
        assert_eq!(body["variables"]["w"], "ws123");
        assert_eq!(body["variables"]["v0"], "AAA111");
        assert_eq!(body["variables"]["v1"], "bbb-222");
        assert!(!query.contains("AAA111"), "id must travel via variables");
    }

    #[test]
    fn titles_request_rejects_malformed_doc_id() {
        let ids = vec!["ok123".to_string(), "bad id\"; drop".to_string()];

        let err = build_titles_request("ws123", &ids);

        assert!(err.is_err());
    }

    #[test]
    fn titles_request_rejects_empty_id_list() {
        let err = build_titles_request("ws123", &[]);

        assert!(err.is_err());
    }

    #[test]
    fn payload_maps_missing_docs_to_none() {
        let ids = vec!["AAA111".to_string(), "MISSING".to_string()];
        let payload = json!({
            "data": {"workspace": {
                "d0": {"id": "AAA111", "title": "Задача про таймауты"},
                "d1": {"id": "MISSING", "title": null},
            }}
        });

        let map = parse_titles_payload(&payload, &ids).expect("clean payload");

        assert_eq!(map["AAA111"].as_deref(), Some("Задача про таймауты"));
        assert_eq!(map["MISSING"], None);
    }

    #[test]
    fn payload_reports_graphql_errors_instead_of_partial_map() {
        let ids = vec!["AAA111".to_string()];
        let payload = json!({"errors": [{"message": "Permission denied"}]});

        let err = parse_titles_payload(&payload, &ids);

        assert!(err
            .map_err(|e| e.to_string())
            .err()
            .is_some_and(|msg| msg.contains("Permission denied")));
    }

    #[test]
    fn payload_without_workspace_is_an_error() {
        let ids = vec!["AAA111".to_string()];
        let payload = json!({"data": {"workspace": null}});

        let err = parse_titles_payload(&payload, &ids);

        assert!(err.is_err());
    }

    /// Сетевой контракт: живой POST до mock UI API, промах → None в мапе.
    #[test]
    fn fetch_titles_posts_one_batch_and_maps_placeholders() {
        let batch = json!({
            "data": {"workspace": {
                "d0": {"id": "AAA111", "title": "Задача про таймауты"},
                "d1": {"id": "MISSING", "title": null},
            }}
        })
        .to_string();
        let mock = crate::mock_ui::MockUi::start(vec![
            crate::mock_ui::UiStep::ok("{}".into()),
            crate::mock_ui::UiStep::ok(batch),
        ]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ui = AffineUi::connect(&mock.config()).await.expect("sign-in");
            let titles = fetch_titles(&ui, "ws-test", &["AAA111".into(), "MISSING".into()])
                .await
                .expect("batch resolves");

            assert_eq!(titles["AAA111"].as_deref(), Some("Задача про таймауты"));
            assert_eq!(titles["MISSING"], None);
        });
        assert_eq!(mock.request_count(), 2, "sign-in + один батч-POST");
    }

    /// Ретрай: первый /graphql отвечает 5xx — второй должен пройти.
    #[test]
    fn fetch_titles_retries_once_on_server_error() {
        let batch = json!({
            "data": {"workspace": {"d0": {"id": "AAA111", "title": "Т"}}}
        })
        .to_string();
        let mock = crate::mock_ui::MockUi::start(vec![
            crate::mock_ui::UiStep::ok("{}".into()),
            crate::mock_ui::UiStep::status(500, "boom".into()),
            crate::mock_ui::UiStep::ok(batch),
        ]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ui = AffineUi::connect(&mock.config()).await.expect("sign-in");
            let titles = fetch_titles(&ui, "ws-test", &["AAA111".into()])
                .await
                .expect("retry спасает от 5xx");

            assert_eq!(titles["AAA111"].as_deref(), Some("Т"));
        });
        assert_eq!(
            mock.request_count(),
            3,
            "sign-in + упавший 5xx + успешный повтор"
        );
    }

    /// Исчерпание ретрая: оба POST — 5xx → честная ошибка, не паника.
    #[test]
    fn fetch_titles_fails_after_exhausted_retries() {
        let mock = crate::mock_ui::MockUi::start(vec![
            crate::mock_ui::UiStep::ok("{}".into()),
            crate::mock_ui::UiStep::status(500, "boom".into()),
            crate::mock_ui::UiStep::status(500, "boom again".into()),
        ]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ui = AffineUi::connect(&mock.config()).await.expect("sign-in");

            assert!(fetch_titles(&ui, "ws-test", &["AAA111".into()])
                .await
                .is_err());
        });
        assert_eq!(mock.request_count(), 3, "ровно одна повторная попытка");
    }
}
