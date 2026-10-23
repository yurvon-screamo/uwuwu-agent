//! AFFiNE document access: slug → docId resolution and markdown reads.
//! Slugs live as `slug:<slug>` doc tags in the workspace, so ANY machine
//! resolves them without shared files; the local per-instance map is only
//! a write cache. Multi-PC safe by design.

use std::path::Path;

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::affine_client::{AffineClient, AffineConfig};
use crate::affine_store::MigrationMap;

pub fn slug_tag(slug: &str) -> String {
    format!("slug:{slug}")
}

/// Resolve a slug to a docId: local map first, then the workspace by tag.
pub async fn resolve_slug(
    client: &mut AffineClient,
    map: &mut MigrationMap,
    cfg: &AffineConfig,
    slug: &str,
) -> Result<Option<String>> {
    if let Some(id) = map.doc_id(slug) {
        return Ok(Some(id.to_string()));
    }
    let resp = client
        .call_tool(
            "search_docs",
            json!({"workspaceId": cfg.workspace, "query": slug, "tag": slug_tag(slug), "limit": 5}),
        )
        .await?;
    let results = resp.get("results").and_then(|r| r.as_array()).cloned();
    if let Some(doc) = results.unwrap_or_default().into_iter().find_map(|item| {
        let in_trash = item
            .get("inTrash")
            .and_then(|t| t.as_bool())
            .unwrap_or(true);
        let tags = item
            .get("tags")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        let has_tag = tags.iter().any(|t| t.as_str() == Some(&slug_tag(slug)));
        if !in_trash && has_tag {
            item.get("docId").and_then(|d| d.as_str()).map(String::from)
        } else {
            None
        }
    }) {
        map.put_doc(slug, &doc);
        map.save()?;
        return Ok(Some(doc));
    }
    Ok(None)
}

/// Read a document's markdown body.
pub async fn read_markdown(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    doc_id: &str,
) -> Result<String> {
    let resp = client
        .call_tool(
            "read_doc",
            json!({"workspaceId": cfg.workspace, "docId": doc_id, "includeMarkdown": true}),
        )
        .await?;
    // answer shape: markdown field, or nested doc.markdown
    for pointer in ["/markdown", "/doc/markdown"] {
        if let Some(md) = resp.pointer(pointer).and_then(|v| v.as_str()) {
            return Ok(unescape_entities(md));
        }
    }
    Err(anyhow!(
        "read_doc returned no markdown for {doc_id}: {resp:.200}"
    ))
}

/// The gateway HTML-escapes newlines and special chars inside markdown
/// (`&#10;`, `&amp;`, …); restore the raw text.
fn unescape_entities(text: &str) -> String {
    text.replace("&#10;", "\n")
        .replace("&#13;", "\r")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
}

/// Tag a doc with its slug (idempotent on the gateway side).
pub async fn tag_slug(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    doc_id: &str,
    slug: &str,
) -> Result<()> {
    client
        .call_tool(
            "add_tag_to_doc",
            json!({"workspaceId": cfg.workspace, "docId": doc_id, "tag": slug_tag(slug)}),
        )
        .await?;
    Ok(())
}

/// Ensure the "Requests" container exists and return its docId.
pub async fn ensure_requests_container(
    client: &mut AffineClient,
    map: &mut MigrationMap,
    cfg: &AffineConfig,
) -> Result<String> {
    crate::affine_migrate::ensure_container(
        client,
        map,
        &cfg.workspace,
        "Requests",
        "# Requests\n\nОчередь заявок на статьи опыта (обработка → `uwuwu-cli write`).",
    )
    .await
}

/// Backfill `slug:` tags for every mapped document (one-time migration aid).
pub async fn backfill_slug_tags(root: &Path, cfg: &AffineConfig) -> Result<(usize, usize)> {
    let mut client = AffineClient::connect(cfg).await?;
    let map = MigrationMap::load_for(root, cfg)?;
    let mut tagged = 0usize;
    let mut skipped = 0usize;
    for (slug, doc_id) in map.taggable_entries() {
        match tag_slug(&mut client, cfg, &doc_id, &slug).await {
            Ok(()) => tagged += 1,
            Err(e) => {
                eprintln!("warn: tag {slug}: {e:#}");
                skipped += 1;
            }
        }
    }
    Ok((tagged, skipped))
}

#[cfg(test)]
mod tests {
    use super::unescape_entities;

    #[test]
    fn entities_are_restored() {
        assert_eq!(unescape_entities("a&#10;b&amp;c"), "a\nb&c");
        assert_eq!(unescape_entities("plain"), "plain");
    }
}
