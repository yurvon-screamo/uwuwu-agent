//! Local dedup index for the facts pipeline: sha → statement/shingles/meta.
//! This is an operational cache, NOT a knowledge store — every fact lives in
//! AFFiNE daily buckets; the index only powers duplicate/conflict detection
//! between runs. Bootstraps once from legacy FS facts (if present), then the
//! FS tree can be deleted — but ONLY after the index file exists (see
//! `load`). A `.bak` copy keeps one previous generation against corruption.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

const INDEX_FILE: &str = "affine_facts_index.json";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct IndexedFact {
    pub slug: String,
    #[serde(default)]
    pub title: String,
    pub statement: String,
    pub date: String,
    pub sources: Vec<String>,
    pub shingles: HashSet<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub confidence: String,
}

#[derive(Default, Serialize, Deserialize, Debug)]
pub struct FactIndex {
    #[serde(flatten)]
    entries: BTreeMap<String, IndexedFact>,
    /// slug set for O(1) uniqueness checks; rebuilt on load.
    #[serde(skip)]
    by_slug: HashSet<String>,
    /// daily buckets already ingested: day → content hash. A changed hash
    /// (appends from other machines) re-ingests the day — cross-PC dedup
    /// sees appends, not just new days.
    #[serde(default)]
    synced_days: BTreeMap<String, String>,
}

impl FactIndex {
    /// Load for interactive sessions: a missing index with no legacy facts
    /// is fine — the AFFiNE bucket sync will populate it (multi-PC entry
    /// point). Corruption still fails loudly.
    pub fn load_or_bootstrap(root: &Path) -> Result<Self> {
        match Self::load(root) {
            Ok(index) => Ok(index),
            Err(e) if e.to_string().contains("no legacy facts") => {
                eprintln!("affine: facts index missing — bootstrapping from daily buckets");
                Ok(Self::default())
            }
            Err(e) => Err(e),
        }
    }

    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(INDEX_FILE);
        if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            let mut index: FactIndex = serde_json::from_str(&raw).with_context(|| {
                format!(
                    "parse {} — corrupt index kills dedup; restore the .bak copy \
                     or remove the file to reset (duplicates will follow)",
                    path.display()
                )
            })?;
            index.rebuild_slug_set();
            return Ok(index);
        }
        // The index is the ONLY dedup state. A missing file with no legacy
        // facts to bootstrap from is silent dedup loss — refuse loudly,
        // unless the operator explicitly opts into a reset.
        let legacy = root.join("facts");
        let reset = std::env::var("UWUWU_FACTS_INDEX_RESET").ok().as_deref() == Some("1");
        if !legacy.exists() {
            if reset {
                let index = FactIndex::default();
                index.save(root)?;
                return Ok(index);
            }
            return Err(anyhow!(
                "{} missing and no legacy facts/ to bootstrap from — the dedup \
                 state is lost. Restore a backup or set UWUWU_FACTS_INDEX_RESET=1 \
                 to start an empty index (duplicate facts will be published)",
                path.display()
            ));
        }
        let mut index = FactIndex::default();
        index.bootstrap_from_fs(&legacy);
        index.save(root)?;
        Ok(index)
    }

    fn rebuild_slug_set(&mut self) {
        self.by_slug = self.entries.values().map(|f| f.slug.clone()).collect();
    }

    fn bootstrap_from_fs(&mut self, legacy: &Path) {
        let mut files = Vec::new();
        walk_md(legacy, &mut files);
        files.sort();
        for path in files {
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            let (fm, statement) = crate::affine_store::split_frontmatter(&raw);
            if statement.is_empty() {
                continue;
            }
            let slug = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let title = fm.title.clone().unwrap_or_else(|| slug.clone());
            let date = fm.updated.clone().unwrap_or_else(|| month_fallback(&path));
            let sha = sha1_norm(&statement);
            self.entries.entry(sha).or_insert_with(|| IndexedFact {
                slug,
                title,
                statement,
                date,
                sources: fm.sources,
                shingles: Default::default(),
                conflicts: Vec::new(),
                status: fm.status.unwrap_or_default(),
                tags: fm.tags,
                confidence: fm.confidence.unwrap_or_default(),
            });
        }
        for fact in self.entries.values_mut() {
            if fact.shingles.is_empty() {
                fact.shingles = shingles(&fact.statement);
            }
        }
    }

    pub fn get_mut(&mut self, sha: &str) -> Option<&mut IndexedFact> {
        self.entries.get_mut(sha)
    }

    pub fn slug_exists(&self, slug: &str) -> bool {
        self.by_slug.contains(slug)
    }

    pub fn insert_new(&mut self, sha: String, fact: IndexedFact) {
        self.by_slug.insert(fact.slug.clone());
        self.entries.insert(sha, fact);
    }

    /// Persist atomically, keeping one previous generation in `.bak`.
    pub fn save(&self, root: &Path) -> Result<()> {
        let path: PathBuf = root.join(INDEX_FILE);
        let json = serde_json::to_string(&self).with_context(|| "serialize facts index")?;
        if path.exists() {
            if let Err(e) = std::fs::copy(&path, path.with_extension("bak")) {
                eprintln!("warn: facts index backup copy failed: {e}");
            }
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &IndexedFact)> {
        self.entries.iter()
    }
}

fn walk_md(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_md(&path, out);
        } else if path.extension().map(|x| x == "md").unwrap_or(false) {
            out.push(path);
        }
    }
}

fn month_fallback(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| format!("{}-01", n.to_string_lossy()))
        .unwrap_or_else(|| "1970-01-01".into())
}

/// sha1 over NFKC-lowercased whitespace-normalized text (legacy parity).
pub(crate) fn sha1_norm(text: &str) -> String {
    use sha1::{Digest as _, Sha1};
    use unicode_normalization::UnicodeNormalization;
    let norm: String = text
        .nfkc()
        .flat_map(|c| c.to_lowercase())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut h = Sha1::new();
    h.update(norm.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Word bigrams over [a-zа-яё0-9]+ tokens (legacy parity).
pub(crate) fn shingles(text: &str) -> HashSet<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"[a-zа-яё0-9]+").unwrap());
    let words: Vec<String> = re
        .find_iter(&text.to_lowercase())
        .map(|m| m.as_str().to_string())
        .collect();
    let mut set = HashSet::new();
    for i in 0..words.len().saturating_sub(1).max(1) {
        if i + 1 < words.len() {
            set.insert(format!("{} {}", words[i], words[i + 1]));
        } else if words.len() == 1 {
            set.insert(words[i].clone());
        }
    }
    set
}

#[derive(Default, Debug, PartialEq)]
pub struct BucketSyncReport {
    pub buckets_read: usize,
    pub facts_indexed: usize,
}

impl FactIndex {
    /// Cross-PC dedup: pull facts from the workspace daily buckets into the
    /// local index. Facts created on other machines are invisible locally
    /// otherwise. `bucket_slugs` = (date, docId, markdown) tuples for buckets
    /// not yet synced (tracked via `synced_days`).
    pub fn ingest_buckets(&mut self, buckets: &[(String, String, String)]) -> BucketSyncReport {
        let mut report = BucketSyncReport::default();
        for (date, doc_id, markdown) in buckets {
            let hash = content_hash(markdown);
            if self.is_day_current(date, &hash) {
                continue;
            }
            report.buckets_read += 1;
            let before = self.entries.len();
            for fact in parse_fact_sections(markdown) {
                let statement = fact.statement.clone();
                let sha = sha1_norm(&statement);
                let shingles = shingles(&statement);
                let slug_clone = fact.slug.clone();
                self.entries.entry(sha).or_insert_with(|| IndexedFact {
                    slug: fact.slug,
                    title: fact.title,
                    statement,
                    date: fact.date.unwrap_or_else(|| date.clone()),
                    sources: fact.sources,
                    shingles,
                    conflicts: Vec::new(),
                    status: fact.status,
                    tags: fact.tags,
                    confidence: fact.confidence,
                });
                self.by_slug.insert(slug_clone);
                let _ = doc_id;
            }
            report.facts_indexed += self.entries.len() - before;
            self.synced_days.insert(date.clone(), hash);
        }
        report
    }

    /// True when the day is ingested at this exact content hash.
    pub fn is_day_current(&self, date: &str, content_hash: &str) -> bool {
        self.synced_days.get(date).map(String::as_str) == Some(content_hash)
    }

    /// True when the day was ingested at any content (first-sync tracker).
    pub fn day_known(&self, date: &str) -> bool {
        self.synced_days.contains_key(date)
    }
}

/// One section of a daily bucket rendered by `render_fact_section`.
#[derive(Default, Debug)]
struct BucketFact {
    slug: String,
    title: String,
    statement: String,
    date: Option<String>,
    sources: Vec<String>,
    tags: Vec<String>,
    confidence: String,
    status: String,
}

/// Parse `render_fact_section` output back into structured facts. Our own
/// format is the contract; unknown lines land in the statement.
fn parse_fact_sections(markdown: &str) -> Vec<BucketFact> {
    let mut facts = Vec::new();
    let mut current: Option<BucketFact> = None;
    let mut statement_lines: Vec<String> = Vec::new();
    for line in markdown.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            // a heading inside a statement (statement containing "## ")
            // must not split the section — only completed sections split
            let already_closed = current.as_ref().is_some_and(|f| !f.slug.is_empty());
            if already_closed || current.is_none() {
                flush(&mut current, &mut statement_lines, &mut facts);
                current = Some(BucketFact {
                    title: title.trim().to_string(),
                    ..Default::default()
                });
                continue;
            }
        }
        if let Some(fact) = current.as_mut() {
            let trimmed = line.trim();
            let is_meta =
                trimmed.starts_with('*') && trimmed.ends_with('*') && trimmed.contains("slug: ");
            if is_meta {
                let meta = trimmed.trim_start_matches('*').trim_end_matches('*');
                ingest_meta(fact, meta);
            } else if !trimmed.is_empty() || !statement_lines.is_empty() {
                statement_lines.push(line.to_string());
            }
        }
    }
    flush(&mut current, &mut statement_lines, &mut facts);
    facts
}

fn flush(
    current: &mut Option<BucketFact>,
    statement_lines: &mut Vec<String>,
    facts: &mut Vec<BucketFact>,
) {
    if let Some(mut fact) = current.take() {
        fact.statement = statement_lines.join("\n").trim().to_string();
        statement_lines.clear();
        if !fact.statement.is_empty() && !fact.slug.is_empty() {
            facts.push(fact);
        }
    }
    statement_lines.clear();
}

fn ingest_meta(fact: &mut BucketFact, meta: &str) {
    for part in meta.split(" · ") {
        let part = part.trim();
        if let Some(tags) = part.strip_prefix("conf ") {
            fact.confidence = tags.to_string();
        } else if let Some(rest) = part.strip_prefix("sources: ") {
            fact.sources = rest.split(", ").map(String::from).collect();
        } else if let Some(slug) = part.strip_prefix("slug: ") {
            fact.slug = slug.to_string();
        } else if part.contains("СТАТУС: КОНФЛИКТ") {
            fact.status = "conflict".into();
        } else if !part.is_empty() && !part.starts_with("fact") && !part.contains(':') {
            fact.tags = part.split(", ").map(String::from).collect();
        }
    }
}

fn content_hash(text: &str) -> String {
    use sha1::{Digest as _, Sha1};
    let mut h = Sha1::new();
    h.update(text.as_bytes());
    h.update((text.len() as u64).to_le_bytes().as_slice());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod bucket_tests {
    use super::*;

    #[test]
    fn sections_roundtrip_through_render_and_parse() {
        let entry = crate::affine_store::FactEntry {
            slug: "affine-port".into(),
            date: "2026-10-05".into(),
            title: "Affine port".into(),
            statement: "Selfhost listens on 3010.".into(),
            tags: vec!["affine".into()],
            confidence: "0.9".into(),
            sources: vec!["manual/x".into()],
            status: "accepted".into(),
        };
        let markdown = crate::affine_store::render_fact_section(&entry);
        let parsed = parse_fact_sections(&markdown);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].slug, "affine-port");
        assert_eq!(parsed[0].statement, "Selfhost listens on 3010.");
        assert_eq!(parsed[0].tags, vec!["affine"]);
        assert_eq!(parsed[0].confidence, "0.9");
        assert_eq!(parsed[0].sources, vec!["manual/x"]);
    }

    #[test]
    fn markdown_hostile_statements_survive_roundtrip() {
        let entry = crate::affine_store::FactEntry {
            slug: "tricky".into(),
            date: "2026-10-23".into(),
            title: "Tricky".into(),
            statement: "Line one.\n## looks like a heading\n*looks like meta*\nLine last.".into(),
            tags: vec![],
            confidence: "0.5".into(),
            sources: vec!["s".into()],
            status: "accepted".into(),
        };
        let markdown = crate::affine_store::render_fact_section(&entry);
        let parsed = parse_fact_sections(&markdown);
        assert_eq!(parsed.len(), 1, "one section, not split");
        assert!(parsed[0].statement.contains("looks like a heading"));
        assert!(parsed[0].statement.contains("Line last."));
        assert_eq!(parsed[0].slug, "tricky");
    }

    #[test]
    fn ingest_buckets_skips_unchanged_days_and_sees_appends() {
        let markdown = "## A\n\nStatement A here.\n\n*tag1 · conf 0.8 · sources: s1 · slug: a*\n";
        let mut index = FactIndex::default();
        let r1 = index.ingest_buckets(&[("2026-10-23".into(), "d1".into(), markdown.into())]);
        assert_eq!(r1.facts_indexed, 1);
        // identical content → skipped entirely
        let r2 = index.ingest_buckets(&[("2026-10-23".into(), "d1".into(), markdown.into())]);
        assert_eq!(r2.buckets_read, 0);
        // another machine appended a fact → the day is re-read and the new
        // fact lands in the index (cross-PC dedup over the append path)
        let appended = format!(
            "{markdown}## B\n\nStatement B here.\n\n*t · conf 0.9 · sources: s2 · slug: b*\n"
        );
        let r3 = index.ingest_buckets(&[("2026-10-23".into(), "d1".into(), appended)]);
        assert_eq!(r3.buckets_read, 1);
        assert_eq!(r3.facts_indexed, 1);
        assert!(index.slug_exists("a"));
        assert!(index.slug_exists("b"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_bootstraps_from_legacy_facts_once() {
        let dir = tempfile::tempdir().unwrap();
        let facts = dir.path().join("facts/2026-06");
        std::fs::create_dir_all(&facts).unwrap();
        std::fs::write(
            facts.join("alpha.md"),
            "---\nupdated: 2026-06-02\ntitle: A\nsources: [manual/2026-06-02]\ntags: [t1]\nconfidence: 0.9\n---\n\nAlpha statement here",
        )
        .unwrap();
        let index = FactIndex::load(dir.path()).unwrap();
        assert_eq!(index.iter().count(), 1);
        let (_, fact) = index.iter().next().unwrap();
        assert_eq!(fact.title, "A");
        assert_eq!(fact.tags, vec!["t1"]);
        assert_eq!(fact.confidence, "0.9");
        // second load comes from the index file, not the FS
        std::fs::remove_dir_all(dir.path().join("facts")).unwrap();
        let again = FactIndex::load(dir.path()).unwrap();
        assert_eq!(again.iter().count(), 1);
    }

    #[test]
    #[serial_test::serial]
    fn missing_index_without_legacy_facts_is_loud() {
        let dir = tempfile::tempdir().unwrap();
        let err = FactIndex::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("dedup"));
        // explicit reset opt-in creates an empty index
        std::env::set_var("UWUWU_FACTS_INDEX_RESET", "1");
        let empty = FactIndex::load(dir.path()).unwrap();
        std::env::remove_var("UWUWU_FACTS_INDEX_RESET");
        assert_eq!(empty.iter().count(), 0);
    }

    #[test]
    fn corrupt_index_fails_loudly() {
        let dir = tempfile::tempdir().unwrap();
        let facts = dir.path().join("facts/2026-06");
        std::fs::create_dir_all(&facts).unwrap();
        std::fs::write(
            facts.join("x.md"),
            "---\nupdated: 2026-06-02\n---\n\nStatement x",
        )
        .unwrap();
        let index = FactIndex::load(dir.path()).unwrap();
        // load() saved once during bootstrap; re-create the clean slate and
        // exercise two explicit saves to observe .bak generation
        std::fs::remove_file(dir.path().join(INDEX_FILE)).unwrap();
        index.save(dir.path()).unwrap();
        let bak = dir.path().join("affine_facts_index.bak");
        assert!(!bak.exists(), "no .bak when nothing to back up");
        index.save(dir.path()).unwrap();
        assert!(bak.exists(), ".bak keeps the previous generation");
        std::fs::write(dir.path().join(INDEX_FILE), "{broken").unwrap();
        assert!(FactIndex::load(dir.path()).is_err());
    }

    #[test]
    fn slug_and_insert_roundtrip_with_slug_set() {
        let dir = tempfile::tempdir().unwrap();
        let mut index = FactIndex::default();
        assert!(!index.slug_exists("beta"));
        index.insert_new(
            "sha-1".into(),
            IndexedFact {
                slug: "beta".into(),
                title: "Beta".into(),
                statement: "Beta statement".into(),
                date: "2026-10-23".into(),
                sources: vec!["manual/x".into()],
                shingles: shingles("Beta statement"),
                conflicts: vec![],
                status: String::new(),
                tags: vec![],
                confidence: String::new(),
            },
        );
        index.save(dir.path()).unwrap();
        let reloaded = FactIndex::load(dir.path()).unwrap();
        assert!(reloaded.slug_exists("beta"));
        assert!(reloaded.iter().any(|(k, _)| k == "sha-1"));
    }
}
