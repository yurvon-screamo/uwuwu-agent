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
    let mut client = AffineClient::connect(cfg).await?;
    let mut map = MigrationMap::load(root)?;
    let mut published = 0usize;
    for (date, day_facts) in group_by_date(facts) {
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
            let bucket_id = doc_id_from_response(&resp)
                .ok_or_else(|| anyhow!("daily bucket {date} created but no docId returned"))?;
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
