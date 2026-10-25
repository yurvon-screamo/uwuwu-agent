//! Local content cache over the AFFiNE workspace: the only place grep and
//! semantic search read from. Source of truth is always the workspace; each
//! machine keeps its own cache (multi-PC safe — read-only mirror).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail as cache_bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::affine_client::{AffineClient, AffineConfig};

const CACHE_FILE: &str = "affine_content_cache.json";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum DocKind {
    Experience,
    Project,
    Daily,
    Request,
    Other,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CachedDoc {
    pub doc_id: String,
    pub title: String,
    pub kind: DocKind,
    pub markdown: String,
}

#[derive(Default, Serialize, Deserialize)]
pub struct ContentCache {
    docs: BTreeMap<String, CachedDoc>, // slug (or synthetic id) → doc
    /// epoch seconds of the last successful sync (session TTL).
    #[serde(default)]
    last_synced_at: u64,
}

impl ContentCache {
    /// Iterate all cached docs as (slug, doc).
    pub fn iter(&self) -> impl Iterator<Item = (&String, &CachedDoc)> {
        self.docs.iter()
    }

    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(CACHE_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("parse {} — remove the file to resync", path.display()))
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path = root.join(CACHE_FILE);
        let json = serde_json::to_string(&self).context("serialize content cache")?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn get(&self, slug: &str) -> Option<&CachedDoc> {
        self.docs.get(slug)
    }

    pub fn docs_by_kind(&self, kind: &DocKind) -> Vec<(&String, &CachedDoc)> {
        self.docs.iter().filter(|(_, d)| d.kind == *kind).collect()
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    pub fn put(&mut self, slug: String, doc: CachedDoc) {
        self.docs.insert(slug, doc);
    }

    /// Sync from the workspace: one tree walk classifies docs into kinds;
    /// unknown docIds are fetched with their markdown.
    pub async fn sync(
        &mut self,
        client: &mut AffineClient,
        cfg: &AffineConfig,
        root: &Path,
    ) -> Result<SyncReport> {
        let mut report = SyncReport::default();
        let tree = client
            .call_tool(
                "list_workspace_tree",
                json!({"workspaceId": cfg.workspace, "depth": 3}),
            )
            .await?;
        let nodes = flatten_tree(&tree);
        if nodes.is_empty() {
            // an unrecognized answer shape must NOT wipe the cache
            cache_bail!(
                "workspace tree came back empty — cache left untouched, run `uwuwu-cli refresh`"
            );
        }
        // Truncated-tree guard: the gateway nondeterministically returns a
        // partial tree under session load (caught live twice on 24.10:
        // ~250 of 848 nodes, exit 0, Daily/Projects/Requests silently
        // wiped from the cache). A real workspace never shrinks 20%+ in
        // one sync — bail instead of letting retain() cut live docs.
        if !self.docs.is_empty() && tree_looks_truncated(nodes.len(), self.docs.len()) {
            cache_bail!(
                "workspace tree truncated: {} nodes vs {} cached (gateway flake?) — \
                 cache left untouched, rerun `uwuwu-cli refresh`",
                nodes.len(),
                self.docs.len()
            );
        }
        for node in &nodes {
            let kind = node.kind.clone();
            let slug = node.slug.clone();
            if self.docs.contains_key(&slug) {
                continue;
            }
            let markdown = super::affine_docs::read_markdown(client, cfg, &node.doc_id).await?;
            report.fetched += 1;
            if report.fetched.is_multiple_of(50) {
                eprintln!("  … fetched {}", report.fetched);
                self.save(root)?;
            }
            self.docs.insert(
                slug.clone(),
                CachedDoc {
                    doc_id: node.doc_id.clone(),
                    title: node.title.clone(),
                    kind,
                    markdown,
                },
            );
        }
        // Drop entries the workspace no longer has (trashed/deleted docs).
        // Deliberately AFTER the fetch loop: a mid-sync failure (`?` above)
        // or a flaky gateway must never leave a cache with live docs cut —
        // intermediate saves only ever ADD fetched docs on top of the old
        // state, removals happen once the whole cycle succeeded.
        let live_ids: std::collections::HashSet<&str> =
            nodes.iter().map(|n| n.doc_id.as_str()).collect();
        self.docs
            .retain(|_, doc| live_ids.contains(doc.doc_id.as_str()));
        self.last_synced_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.save(root)?;
        Ok(report)
    }

    /// Fresh enough for an interactive command? Avoids two tree walks per
    /// single `read`/`grep` when the cache was synced moments ago.
    pub fn is_fresh(&self, max_age_secs: u64) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.last_synced_at != 0 && now.saturating_sub(self.last_synced_at) < max_age_secs
    }
}

#[derive(Default, Debug)]
pub struct SyncReport {
    pub fetched: usize,
}

struct TreeNode {
    doc_id: String,
    title: String,
    slug: String,
    kind: DocKind,
}

fn flatten_tree(resp: &serde_json::Value) -> Vec<TreeNode> {
    let mut out = Vec::new();
    // answer: { roots: [ { doc: {id,title}, children: [...] } ] } — walk
    // defensively: find every object with a docId/title pair.
    walk_nodes(resp, &mut Vec::new(), &mut out);
    out
}

/// A partial tree from the gateway must not look like mass deletions:
/// if the node count dropped below 80% of the cached size, assume a
/// truncation flake rather than real workspace shrinkage.
fn tree_looks_truncated(nodes: usize, cached: usize) -> bool {
    (nodes as f64) < 0.8 * (cached as f64)
}

fn walk_nodes(value: &serde_json::Value, path: &mut Vec<String>, out: &mut Vec<TreeNode>) {
    match value {
        serde_json::Value::Object(map) => {
            let doc_id = map.get("docId").and_then(|v| v.as_str()).map(String::from);
            let title = map.get("title").and_then(|v| v.as_str()).map(String::from);
            if let (Some(doc_id), Some(title)) = (doc_id, title) {
                let kind = classify(path, &title);
                let slug = slug_for(path, &title, &kind);
                out.push(TreeNode {
                    doc_id,
                    title,
                    slug,
                    kind,
                });
            }
            for (key, child) in map {
                if key == "children" || key == "roots" || key == "nodes" {
                    if let serde_json::Value::Array(items) = child {
                        let label = map.get("title").and_then(|v| v.as_str()).unwrap_or("");
                        for item in items {
                            let mut next = path.clone();
                            if key != "roots" {
                                next.push(label.to_string());
                            }
                            walk_nodes(item, &mut next, out);
                        }
                    }
                } else if child.is_object() || child.is_array() {
                    let mut next = path.clone();
                    if let Some(label) = map.get("title").and_then(|v| v.as_str()) {
                        next.push(label.to_string());
                    }
                    walk_nodes(child, &mut next, out);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                walk_nodes(item, path, out);
            }
        }
        _ => {}
    }
}

fn classify(path: &[String], title: &str) -> DocKind {
    if path.first().map(String::as_str) == Some("Experience") {
        return DocKind::Experience;
    }
    if path.first().map(String::as_str) == Some("Projects") {
        return DocKind::Project;
    }
    if path.first().map(String::as_str) == Some("Requests") {
        return DocKind::Request;
    }
    let digits = title.len() == 10
        && title.as_bytes().get(4) == Some(&b'-')
        && title.as_bytes().get(7) == Some(&b'-')
        && title.bytes().all(|b| b.is_ascii_digit() || b == b'-');
    if digits {
        return DocKind::Daily;
    }
    DocKind::Other
}

fn slug_for(path: &[String], title: &str, kind: &DocKind) -> String {
    match kind {
        DocKind::Project => match path.get(1) {
            Some(project) => format!("{project}/{title}"),
            None => title.to_string(),
        },
        _ => title.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_titles_are_classified_by_shape() {
        assert_eq!(classify(&[], "2026-10-23"), DocKind::Daily);
        assert_eq!(classify(&[], "2026-13-99"), DocKind::Daily); // shape only
        assert_ne!(classify(&[], "Just a note"), DocKind::Daily);
    }

    #[test]
    fn containers_classify_children() {
        assert_eq!(
            classify(&["Experience".into()], "axum"),
            DocKind::Experience
        );
        assert_eq!(
            classify(&["Projects".into(), "origa".into()], "topology"),
            DocKind::Project
        );
    }

    #[test]
    fn project_slug_namespaces_by_project() {
        assert_eq!(
            slug_for(
                &["Projects".into(), "origa".into()],
                "topology",
                &DocKind::Project
            ),
            "origa/topology"
        );
    }

    #[test]
    fn cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = ContentCache::default();
        cache.put(
            "axum".into(),
            CachedDoc {
                doc_id: "d1".into(),
                title: "Axum".into(),
                kind: DocKind::Experience,
                markdown: "# Axum".into(),
            },
        );
        cache.save(dir.path()).unwrap();
        let again = ContentCache::load(dir.path()).unwrap();
        assert_eq!(again.get("axum").unwrap().doc_id, "d1");
        assert_eq!(again.docs_by_kind(&DocKind::Experience).len(), 1);
    }

    #[test]
    fn truncated_tree_is_detected() {
        // 250 nodes vs 848 cached — the live 24.10 incident shape
        assert!(tree_looks_truncated(250, 848));
        // mild shrinkage is normal (a few docs deleted)
        assert!(!tree_looks_truncated(840, 848));
        assert!(!tree_looks_truncated(850, 848));
        // empty cache can't be truncated against itself
        assert!(!tree_looks_truncated(0, 0));
    }
}
