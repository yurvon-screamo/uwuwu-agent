//! AFFiNE document access: slug → docId resolution and markdown reads.
//! Addressing: titles via the content cache on every machine; kebab-slugs
//! via the local migration map (machines that ran `affine --all`). Slug TAGS
//! were removed from the workspace — `resolve_slug`'s tag fallback always
//! misses on the current production workspace and stays only for legacy
//! compatibility.

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::affine_client::{AffineClient, AffineConfig};
use crate::affine_store::MigrationMap;

pub fn slug_tag(slug: &str) -> String {
    // bare slug, no prefix — folders handle organization, tags handle addressing
    slug.to_string()
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
    // The gateway requires a non-empty full-text query beside the tag
    // filter; try progressively broader queries and match the exact tag
    // client-side so the tag wins regardless of the text query.
    let tag = slug_tag(slug);
    let tail = slug.rsplit('/').next().unwrap_or(slug);
    for query in [slug, tail, "e"] {
        let resp = client
            .call_tool(
                "search_docs",
                json!({"workspaceId": cfg.workspace, "query": query, "tag": tag, "limit": 20}),
            )
            .await;
        let resp = match resp {
            Ok(resp) => resp,
            Err(_) => continue,
        };
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
            let has_tag = tags.iter().any(|t| t.as_str() == Some(tag.as_str()));
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

#[cfg(test)]
mod tests {
    use super::unescape_entities;

    #[test]
    fn entities_are_restored() {
        assert_eq!(unescape_entities("a&#10;b&amp;c"), "a\nb&c");
        assert_eq!(unescape_entities("plain"), "plain");
    }
}
