//! Daily-bucket publishing: new facts land in the AFFiNE daily document for
//! their date (fact = entity, daily doc = bucket), with a native Journal
//! binding. Bucket ids are recorded in the map before journal binding so a
//! journal failure cannot cause duplicate buckets.

use std::path::Path;

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::affine_client::{AffineClient, AffineConfig};
use crate::affine_collect::group_by_date;
use crate::affine_store::{doc_id_from_response, render_fact_section, FactEntry, MigrationMap};

/// Publish freshly extracted facts of one day into its daily bucket.
/// Called from the facts pipeline outside the pipeline lock; the map is
/// checkpointed after every day.
pub async fn publish_facts(root: &Path, cfg: &AffineConfig, facts: &[FactEntry]) -> Result<usize> {
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
    let bucket_id = match map.daily_bucket(date) {
        Some(id) => {
            client
                .call_tool(
                    "append_markdown",
                    json!({"workspaceId": workspace, "docId": id, "markdown": markdown}),
                )
                .await?;
            id.to_string()
        }
        None => {
            let resp = client
                .call_tool(
                    "create_doc_from_markdown",
                    json!({"workspaceId": workspace, "title": date, "markdown": markdown}),
                )
                .await?;
            let id = doc_id_from_response(&resp)
                .ok_or_else(|| anyhow!("daily bucket {date} created but no docId returned"))?;
            map.put_daily_bucket(date, &id);
            map.save()?;
            client
                .call_tool(
                    "set_doc_journal",
                    json!({"workspaceId": workspace, "docId": id, "date": date}),
                )
                .await?;
            id
        }
    };
    let count = fresh.len();
    // docId of a fact is its bucket; slugs stay addressable via the map
    for f in &fresh {
        map.put_doc(&f.slug, &bucket_id);
    }
    Ok(count)
}
