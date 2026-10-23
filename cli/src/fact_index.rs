//! Local dedup index for the facts pipeline: sha → statement/shingles/meta.
//! This is an operational cache, NOT a knowledge store — every fact lives in
//! AFFiNE daily buckets; the index only powers duplicate/conflict detection
//! between runs. Bootstraps once from legacy FS facts (if present), then the
//! FS tree can be deleted.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const INDEX_FILE: &str = "affine_facts_index.json";

#[derive(Serialize, Deserialize, Clone)]
pub struct IndexedFact {
    pub slug: String,
    #[serde(default)]
    pub title: String,
    pub statement: String,
    pub date: String,
    pub sources: Vec<String>,
    pub shingles: HashSet<String>,
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub status: String,
}

#[derive(Default, Serialize, Deserialize)]
pub struct FactIndex {
    #[serde(flatten)]
    entries: BTreeMap<String, IndexedFact>,
}

impl FactIndex {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(INDEX_FILE);
        if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            // A corrupt index silently kills dedup and resurrects garbage —
            // fail loudly instead.
            return serde_json::from_str(&raw)
                .with_context(|| format!("parse {}: fix or remove the file", path.display()));
        }
        // one-time bootstrap from legacy FS facts, then never touch them again
        let mut index = FactIndex::default();
        index.bootstrap_from_fs(root);
        index.save(root)?;
        Ok(index)
    }

    fn bootstrap_from_fs(&mut self, root: &Path) {
        let facts_dir = root.join("facts");
        if !facts_dir.exists() {
            return;
        }
        let mut files = Vec::new();
        walk_md(&facts_dir, &mut files);
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
            let sha = sha1_norm(&statement);
            let title = fm.title.clone().unwrap_or_else(|| slug.clone());
            self.entries.entry(sha).or_insert_with(|| IndexedFact {
                slug,
                title,
                statement,
                date: fm.updated.unwrap_or_default(),
                sources: fm.sources,
                shingles: Default::default(),
                conflicts: Vec::new(),
                status: fm.status.unwrap_or_default(),
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
        self.entries.values().any(|f| f.slug == slug)
    }

    pub fn insert_new(&mut self, sha: String, fact: IndexedFact) {
        self.entries.insert(sha, fact);
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path: PathBuf = root.join(INDEX_FILE);
        let json = serde_json::to_string(&self).with_context(|| "serialize facts index")?;
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
            "---\nupdated: 2026-06-02\ntitle: A\nsources: [manual/2026-06-02]\n---\n\nAlpha statement here",
        ).unwrap();
        let index = FactIndex::load(dir.path()).unwrap();
        assert_eq!(index.iter().count(), 1);
        // second load comes from the index file, not the FS
        std::fs::remove_dir_all(dir.path().join("facts")).unwrap();
        let again = FactIndex::load(dir.path()).unwrap();
        assert_eq!(again.iter().count(), 1);
    }

    #[test]
    fn corrupt_index_fails_loudly() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(INDEX_FILE), "{broken").unwrap();
        assert!(FactIndex::load(dir.path()).is_err());
    }

    #[test]
    fn slug_and_insert_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut index = FactIndex::load(dir.path()).unwrap();
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
            },
        );
        index.save(dir.path()).unwrap();
        let reloaded = FactIndex::load(dir.path()).unwrap();
        assert!(reloaded.slug_exists("beta"));
        assert!(reloaded.iter().any(|(k, _)| k == "sha-1"));
    }
}
