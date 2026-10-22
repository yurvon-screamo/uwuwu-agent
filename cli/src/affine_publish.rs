//! Daily-bucket publishing: new facts land in the AFFiNE daily document for
//! their date (fact = entity, daily doc = bucket), with a native Journal
//! binding. Facts and bucket ids are checkpointed before the journal binding
//! so a journal failure cannot cause duplicate sections or buckets.

use std::path::Path;

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::affine_client::{AffineClient, AffineConfig};
use crate::affine_collect::group_by_date;
use crate::affine_store::{doc_id_from_response, render_fact_section, FactEntry, MigrationMap};
use crate::facts::LockGuard;

/// Publish freshly extracted facts of one day into its daily bucket.
/// Called from the facts pipeline outside the pipeline lock; the map is
/// checkpointed after every day. Holds affine_map.lock for the whole run:
/// concurrent publishers and the migration must not lose map updates
/// (network calls inside are fine — the facts pipeline never waits on it).
pub async fn publish_facts(root: &Path, cfg: &AffineConfig, facts: &[FactEntry]) -> Result<usize> {
    let _guard = LockGuard::acquire_at(&root.join("affine_map.lock"))?;
    let mut map = MigrationMap::load_for(root, &cfg.url)?;
    // Skip the gateway session entirely when every fact is already visible
    // in its bucket — a retried run pays nothing for converged state.
    let days: Vec<(String, Vec<&FactEntry>)> = group_by_date(facts)
        .into_iter()
        .filter(|(_, day)| day.iter().any(|f| map.doc_id(&f.slug).is_none()))
        .collect();
    if days.is_empty() {
        return Ok(0);
    }
    let mut client = AffineClient::connect(cfg).await?;
    let mut published = 0usize;
    for (date, day_facts) in days {
        published +=
            upsert_daily_bucket(&mut client, &mut map, &cfg.workspace, &date, &day_facts).await?;
        map.save()?;
    }
    Ok(published)
}

/// Create the day bucket with `facts` or append the ones not migrated yet.
/// Returns how many facts became visible in the bucket.
pub(crate) async fn upsert_daily_bucket(
    client: &mut AffineClient,
    map: &mut MigrationMap,
    workspace: &str,
    date: &str,
    day_facts: &[&FactEntry],
) -> Result<usize> {
    let fresh: Vec<&FactEntry> = day_facts
        .iter()
        .filter(|f| map.doc_id(&f.slug).is_none())
        .copied()
        .collect();
    if fresh.is_empty() {
        return Ok(0);
    }
    let markdown: String = fresh.iter().map(|f| render_fact_section(f)).collect();
    let existing_bucket = map.daily_bucket(date).map(|s| s.to_string());
    match existing_bucket {
        Some(bucket_id) => {
            client
                .call_tool(
                    "append_markdown",
                    json!({"workspaceId": workspace, "docId": bucket_id, "markdown": markdown}),
                )
                .await?;
            mark_facts(map, &fresh, &bucket_id);
            Ok(fresh.len())
        }
        None => {
            let resp = client
                .call_tool(
                    "create_doc_from_markdown",
                    json!({"workspaceId": workspace, "title": date, "markdown": markdown}),
                )
                .await?;
            // Ambiguous "no docId" answer: the create may have been applied.
            // Look the bucket up by exact title instead of failing — a blind
            // retry would create a second bucket for the same date.
            let bucket_id = match doc_id_from_response(&resp) {
                Some(id) => id,
                None => match find_bucket_by_title(client, workspace, date).await? {
                    Some(id) => id,
                    None => anyhow::bail!("daily bucket {date} created but no docId returned"),
                },
            };
            // bucket first, then the facts — both BEFORE the journal call:
            // a journal failure must leave a consistent, resumable state
            map.put_daily_bucket(date, &bucket_id);
            mark_facts(map, &fresh, &bucket_id);
            map.save()?;
            client
                .call_tool(
                    "set_doc_journal",
                    json!({"workspaceId": workspace, "docId": bucket_id, "date": date}),
                )
                .await?;
            Ok(fresh.len())
        }
    }
}

fn mark_facts(map: &mut MigrationMap, fresh: &[&FactEntry], bucket_id: &str) {
    // docId of a fact is its bucket; slugs stay addressable via the map
    for f in fresh {
        map.put_doc(&f.slug, bucket_id);
    }
}

/// Recover the bucket id when the create answer carries no docId but the
/// document may exist (timeout-ish ambiguity): exact-title search.
async fn find_bucket_by_title(
    client: &mut AffineClient,
    workspace: &str,
    date: &str,
) -> Result<Option<String>> {
    let resp = client
        .call_tool(
            "search_docs",
            json!({"workspaceId": workspace, "query": date, "limit": 10}),
        )
        .await?;
    let results = resp
        .get("results")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    for item in results {
        let title = item.get("title").and_then(|t| t.as_str()).unwrap_or("");
        let in_trash = item
            .get("inTrash")
            .and_then(|t| t.as_bool())
            .unwrap_or(true);
        if title == date && !in_trash {
            if let Some(id) = item.get("docId").and_then(|d| d.as_str()) {
                return Ok(Some(id.to_string()));
            }
        }
    }
    Ok(None)
}
