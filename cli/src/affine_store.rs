//! Migration map: local slug → AFFiNE docId, stored in the wiki root next to
//! the facts sidecars. Powers idempotent migration (rerun skips mapped docs)
//! and daily-bucket reuse for the facts → journal publish hook.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

const MAP_FILE: &str = "affine_map.json";

/// Container/daily keys are prefixed so they can never collide with slugs.
const CONTAINER_PREFIX: &str = "__container_";
const DAILY_PREFIX: &str = "__daily_";

pub struct MigrationMap {
    path: PathBuf,
    entries: BTreeMap<String, String>,
}

impl MigrationMap {
    pub fn load(wiki_root: &Path) -> Result<Self> {
        let path = wiki_root.join(MAP_FILE);
        let entries = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<BTreeMap<String, String>>(&raw).ok())
            .unwrap_or_default();
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

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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

pub fn split_frontmatter(content: &str) -> (Frontmatter, String) {
    let rest = content.strip_prefix("---\n").unwrap_or(content);
    let Some(end) = rest.find("\n---\n") else {
        return (Frontmatter::default(), content.trim().to_string());
    };
    let fm_raw = &rest[..end];
    let body = rest[end + 5..].trim().to_string();
    (parse_frontmatter(fm_raw), body)
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
    format!(
        "## {}\n\n{}\n\n*{} · conf {} · sources: {} · slug: {}*\n",
        fact.title, fact.statement, tags, fact.confidence, sources, fact.slug
    )
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

/// AFFiNE's markdown importer caps tables at 20 rows total; longer tables
/// abort the whole import. Split oversized tables into consecutive chunks
/// (header repeated) so every chunk fits the cap.
pub fn split_long_tables(markdown: &str, max_data_rows: usize) -> String {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        if is_table_row(line) && i + 1 < lines.len() && is_table_delimiter(lines[i + 1]) {
            let header = line.to_string();
            let delimiter = lines[i + 1].to_string();
            let mut j = i + 2;
            let mut data: Vec<&str> = Vec::new();
            while j < lines.len() && is_table_row(lines[j]) {
                data.push(lines[j]);
                j += 1;
            }
            if data.len() > max_data_rows {
                for chunk in data.chunks(max_data_rows) {
                    if !out.is_empty() && !out.last().map(|l| l.is_empty()).unwrap_or(false) {
                        out.push(String::new());
                    }
                    out.push(header.clone());
                    out.push(delimiter.clone());
                    out.extend(chunk.iter().map(|l| l.to_string()));
                }
            } else {
                out.push(header);
                out.push(delimiter);
                out.extend(data.iter().map(|l| l.to_string()));
            }
            i = j;
        } else {
            out.push(line.to_string());
            i += 1;
        }
    }
    out.join("\n")
}

fn is_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.len() > 1
}

fn is_table_delimiter(line: &str) -> bool {
    let t = line.trim();
    is_table_row(t)
        && t.chars()
            .all(|c| c == '|' || c == '-' || c == ':' || c.is_whitespace())
        && t.contains('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_roundtrip_fact_file() {
        let raw = "---\nupdated: 2026-10-05\ntitle: Port 3010 affine\ndescription: d\nconfidence: 0.9\nsources: [manual/2026-10-05, opencode/x]\ntags: [affine, docker]\nstatus: accepted\n---\n\nAffine selfhost listens on 3010.";
        let (fm, body) = split_frontmatter(raw);
        assert_eq!(fm.updated.as_deref(), Some("2026-10-05"));
        assert_eq!(fm.tags, vec!["affine", "docker"]);
        assert_eq!(fm.sources.len(), 2);
        assert_eq!(fm.confidence.as_deref(), Some("0.9"));
        assert!(body.starts_with("Affine selfhost"));
    }

    #[test]
    fn frontmatter_absent_yields_whole_body() {
        let (fm, body) = split_frontmatter("just text");
        assert!(fm.title.is_none());
        assert_eq!(body, "just text");
    }

    #[test]
    fn fact_section_contains_meta_and_statement() {
        let fact = FactEntry {
            slug: "affine-port".into(),
            date: "2026-10-05".into(),
            title: "Affine port".into(),
            statement: "Selfhost listens on 3010.".into(),
            tags: vec!["affine".into()],
            confidence: "0.9".into(),
            sources: vec!["manual/2026-10-05".into()],
        };
        let section = render_fact_section(&fact);
        assert!(section.contains("## Affine port"));
        assert!(section.contains("Selfhost listens on 3010."));
        assert!(section.contains("slug: affine-port"));
    }

    #[test]
    fn long_table_is_split_into_fitting_chunks() {
        let rows: Vec<String> = (1..=45).map(|n| format!("| col{n} | value |")).collect();
        let markdown = format!(
            "# t\n\n| h1 | h2 |\n|---|---|\n{}\n\ntail text",
            rows.join("\n")
        );
        let split = split_long_tables(&markdown, 18);
        let header_count = split.matches("| h1 | h2 |").count();
        assert_eq!(header_count, 3, "45 rows / 18 = 3 chunks");
        assert!(split.contains("tail text"));
        for chunk in split.split("| h1 | h2 |\n|---|---|\n").skip(1) {
            let data_rows = chunk.lines().take_while(|l| is_table_row(l)).count();
            assert!(data_rows <= 18);
        }
    }

    #[test]
    fn short_table_passes_through_untouched() {
        let markdown = "| h |\n|---|\n| a |\n| b |";
        assert_eq!(split_long_tables(markdown, 18), markdown);
    }

    #[test]
    fn non_table_pipes_are_ignored() {
        let markdown = "just `|code|` inline\nand text";
        assert_eq!(split_long_tables(markdown, 18), markdown);
    }
}
