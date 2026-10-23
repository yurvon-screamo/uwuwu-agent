//! Migration map: local slug → AFFiNE docId, stored in the wiki root next to
//! the facts sidecars. Powers idempotent migration (rerun skips mapped docs)
//! and daily-bucket reuse for the facts → journal publish hook.
//! Plus the frontmatter/section primitives shared by collectors and publisher.
//!
//! Slightly over the 200-line module limit: the map struct and the parsing
//! primitives it travels with form one cohesive storage contract; splitting
//! them would leak the map's save/load lifecycle across files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::affine_client::AffineConfig;

const MAP_FILE: &str = "affine_map.json";

/// Container/daily keys are prefixed so they can never collide with slugs.
const CONTAINER_PREFIX: &str = "__container_";
const DAILY_PREFIX: &str = "__daily_";

/// AFFiNE doc titles are truncated to keep the sidebar readable.
pub const AFFINE_DOC_TITLE_MAX_CHARS: usize = 120;

pub struct MigrationMap {
    path: PathBuf,
    entries: BTreeMap<String, String>,
}

impl MigrationMap {
    /// Map file is per-gateway-instance: docIds from one AFFiNE server are
    /// meaningless on another, and a shared map silently skips the whole
    /// migration (seen live: 2679 "skipped" against an empty workspace).
    /// The default local gateway keeps the legacy `affine_map.json` name.
    pub fn load_for(wiki_root: &Path, cfg: &AffineConfig) -> Result<Self> {
        MigrationMap::load_path(&Self::map_path(wiki_root, cfg))
    }

    /// Instance identity = gateway host AND workspace id: two workspaces
    /// behind one gateway share nothing, docIds of one are invalid in the
    /// other (same failure class the per-host split was introduced for).
    fn map_path(wiki_root: &Path, cfg: &AffineConfig) -> PathBuf {
        let host = gateway_host(&cfg.url)
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .take(48)
            .collect::<String>();
        let ws: String = cfg
            .workspace
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(8)
            .collect();
        wiki_root.join(format!("affine_map_{host}-{ws}.json"))
    }

    pub fn load(wiki_root: &Path) -> Result<Self> {
        MigrationMap::load_path(&wiki_root.join(MAP_FILE))
    }

    fn load_path(path: &Path) -> Result<Self> {
        let path = path.to_path_buf();
        if !path.exists() {
            return Ok(Self {
                path,
                entries: BTreeMap::new(),
            });
        }
        // A corrupt map silently resets idempotency to zero and duplicates
        // every document in the workspace — fail loudly instead.
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let entries =
            serde_json::from_str::<BTreeMap<String, String>>(&raw).with_context(|| {
                format!(
                    "parse {} — the map guards against duplicates; \
                 fix or remove the file and rerun the migration",
                    path.display()
                )
            })?;
        Ok(Self { path, entries })
    }

    pub fn doc_id(&self, slug: &str) -> Option<&str> {
        self.entries.get(slug).map(|s| s.as_str())
    }

    pub fn container(&self, name: &str) -> Option<&str> {
        self.entries
            .get(&format!("{CONTAINER_PREFIX}{name}"))
            .map(|s| s.as_str())
    }

    pub fn daily_bucket(&self, date: &str) -> Option<&str> {
        self.entries
            .get(&format!("{DAILY_PREFIX}{date}"))
            .map(|s| s.as_str())
    }

    /// True when the docId is already bound to some slug — a recovered doc
    /// owned by another slug must not be re-bound (write recovery guard).
    pub fn owns_doc(&self, doc_id: &str) -> bool {
        self.entries.values().any(|v| v == doc_id)
    }

    /// (slug, docId) pairs suitable for slug tagging: real slugs only,
    /// facts (value "daily") and service keys excluded.
    pub fn taggable_entries(&self) -> Vec<(String, String)> {
        self.entries
            .iter()
            .filter(|(k, v)| !k.starts_with("__") && v.as_str() != "daily")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn put_doc(&mut self, slug: &str, doc_id: &str) {
        self.entries.insert(slug.to_string(), doc_id.to_string());
    }

    pub fn put_container(&mut self, name: &str, doc_id: &str) {
        self.entries
            .insert(format!("{CONTAINER_PREFIX}{name}"), doc_id.to_string());
    }

    pub fn put_daily_bucket(&mut self, date: &str, doc_id: &str) {
        self.entries
            .insert(format!("{DAILY_PREFIX}{date}"), doc_id.to_string());
    }

    /// Persist atomically (tmp + rename) — concurrent facts runs may write it.
    pub fn save(&self) -> Result<()> {
        let json =
            serde_json::to_string_pretty(&self.entries).with_context(|| "serialize affine map")?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Frontmatter fields the migration cares about, parsed from the simple
/// `key: value` / `key: [a, b]` lines used by experience and fact files.
#[derive(Default)]
pub struct Frontmatter {
    pub title: Option<String>,
    pub updated: Option<String>,
    pub tags: Vec<String>,
    pub confidence: Option<String>,
    pub sources: Vec<String>,
    pub status: Option<String>,
}

/// Split frontmatter from the body. CRLF is normalized first. A body that
/// does not start with `---` is returned as-is (an early horizontal rule
/// must not be mistaken for frontmatter); the candidate block must also look
/// like `key: value` lines before it is treated as frontmatter.
pub fn split_frontmatter(content: &str) -> (Frontmatter, String) {
    let normalized = content.replace("\r\n", "\n");
    let Some(rest) = normalized.strip_prefix("---\n") else {
        return (Frontmatter::default(), normalized.trim().to_string());
    };
    let Some(end) = rest.find("\n---\n") else {
        return (Frontmatter::default(), normalized.trim().to_string());
    };
    let fm_raw = &rest[..end];
    if !looks_like_frontmatter(fm_raw) {
        return (Frontmatter::default(), normalized.trim().to_string());
    }
    let body = rest[end + 5..].trim().to_string();
    (parse_frontmatter(fm_raw), body)
}

fn looks_like_frontmatter(raw: &str) -> bool {
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .all(|l| l.split_once(':').is_some_and(|(k, _)| !k.trim().is_empty()))
}

fn parse_frontmatter(raw: &str) -> Frontmatter {
    let mut fm = Frontmatter::default();
    for line in raw.lines() {
        let Some((key, value)) = line.split_once(": ") else {
            continue;
        };
        let value = value.trim();
        match key {
            "title" => fm.title = Some(value.to_string()),
            "updated" => fm.updated = Some(value.to_string()),
            "status" => fm.status = Some(value.to_string()),
            "confidence" => fm.confidence = Some(value.to_string()),
            "tags" | "sources" => {
                let inner = value.trim_start_matches('[').trim_end_matches(']');
                let list = inner
                    .split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>();
                if key == "tags" {
                    fm.tags = list
                } else {
                    fm.sources = list
                }
            }
            _ => {}
        }
    }
    fm
}

/// A migration unit: one AFFiNE document to create under a container.
pub struct DocToMigrate {
    pub slug: String,
    pub title: String,
    pub markdown: String,
}

/// One fact destined for a daily bucket (kept raw; rendering is separate).
pub struct FactEntry {
    pub slug: String,
    pub date: String,
    pub title: String,
    pub statement: String,
    pub tags: Vec<String>,
    pub confidence: String,
    pub sources: Vec<String>,
    /// "accepted" | "conflict" | "" — surfaced in the bucket section meta.
    pub status: String,
}

/// Render one fact as a markdown section inside the daily bucket.
pub fn render_fact_section(fact: &FactEntry) -> String {
    let tags = if fact.tags.is_empty() {
        "fact".to_string()
    } else {
        fact.tags.join(", ")
    };
    let sources = if fact.sources.is_empty() {
        "-".to_string()
    } else {
        fact.sources.join(", ")
    };
    let status = if fact.status == "conflict" {
        " · СТАТУС: КОНФЛИКТ"
    } else {
        ""
    };
    format!(
        "## {}\n\n{}\n\n*{} · conf {} · sources: {} · slug: {}{}*\n",
        fact.title, fact.statement, tags, fact.confidence, sources, fact.slug, status
    )
}

/// Extract the host from a gateway URL (no url crate needed): strip the
/// scheme, take up to port or path.
fn gateway_host(url: &str) -> String {
    let rest = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => url,
    };
    let end = rest
        .find(':')
        .or_else(|| rest.find('/'))
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

/// Extract a docId from a create-tool response (tries common shapes).
pub fn doc_id_from_response(resp: &Value) -> Option<String> {
    resp.get("docId")
        .or_else(|| resp.get("doc_id"))
        .or_else(|| resp.get("doc").and_then(|d| d.get("id")))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            // tool may answer with a text payload wrapping the same JSON
            let text = resp.get("content")?.get(0)?.get("text")?.as_str()?;
            let parsed: Value = serde_json::from_str(text).ok()?;
            parsed
                .get("docId")
                .or_else(|| parsed.get("doc_id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
}
