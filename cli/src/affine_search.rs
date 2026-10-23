//! Semantic search over the AFFiNE content cache with local Ollama
//! embeddings. Each machine keeps its own embedding cache (operational,
//! read-only mirror); the workspace stays the single source of truth.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::content_cache::{ContentCache, DocKind};
use crate::embed::embed;
use crate::fact_index::FactIndex;

const MODEL: &str = "qwen3-embedding:0.6b";
const CACHE_FILE: &str = "affine_embedding_cache.json";

#[derive(Serialize, Deserialize, Default)]
struct EmbeddingCache {
    /// content-hash → embedding
    vectors: BTreeMap<String, Vec<f32>>,
}

pub struct SearchHit {
    pub score: f32,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub body: String,
    pub kind: DocKind,
}

pub struct SearchOpts {
    pub experience: usize,
    pub facts: usize,
    pub access: usize,
    pub threshold: f32,
}

/// Unified semantic search: experience + project docs from the content
/// cache, facts from the dedup index statements.
pub async fn search(
    root: &Path,
    cache: &ContentCache,
    index: &FactIndex,
    query: &str,
    opts: &SearchOpts,
) -> Result<Vec<SearchHit>> {
    let mut embed_cache = load_cache(root)?;
    let query_vec = embed(MODEL, query).await?;

    let mut scored: Vec<SearchHit> = Vec::new();
    let mut embed_calls = 0usize;

    let mut docs: Vec<(String, String, String, String, DocKind)> = Vec::new();
    for (slug, doc) in cache
        .docs_by_kind(&DocKind::Experience)
        .into_iter()
        .chain(cache.docs_by_kind(&DocKind::Project))
    {
        docs.push((
            slug.clone(),
            doc.title.clone(),
            first_line(&doc.markdown),
            doc.markdown.clone(),
            doc.kind.clone(),
        ));
    }
    for (_, fact) in index.iter() {
        docs.push((
            format!("facts/{}", fact.slug),
            fact.title.clone(),
            fact.statement.chars().take(140).collect(),
            fact.statement.clone(),
            DocKind::Daily,
        ));
    }

    for (slug, title, description, body, kind) in docs {
        let key = content_key(&body);
        let vector = match embed_cache.vectors.get(&key) {
            Some(v) => v.clone(),
            None => {
                let v = embed(MODEL, &body).await?;
                embed_cache.vectors.insert(key, v.clone());
                embed_calls += 1;
                if embed_calls.is_multiple_of(25) {
                    let _ = save_cache(root, &embed_cache);
                }
                v
            }
        };
        let score = cosine(&query_vec, &vector);
        if score >= opts.threshold {
            scored.push(SearchHit {
                score,
                slug,
                title,
                description,
                body,
                kind,
            });
        }
    }
    save_cache(root, &embed_cache)?;
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // section split mirrors the legacy unified search limits
    let mut out = Vec::new();
    let mut exp = 0;
    let mut facts = 0;
    let mut access = 0;
    for hit in scored {
        match hit.kind {
            DocKind::Daily => {
                if facts < opts.facts {
                    facts += 1;
                    out.push(hit);
                }
            }
            DocKind::Project => {
                if access < opts.access {
                    access += 1;
                    out.push(hit);
                }
            }
            _ => {
                if exp < opts.experience {
                    exp += 1;
                    out.push(hit);
                }
            }
        }
    }
    Ok(out)
}

fn first_line(markdown: &str) -> String {
    markdown
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap_or("")
        .chars()
        .take(140)
        .collect()
}

fn content_key(text: &str) -> String {
    use sha1::{Digest as _, Sha1};
    let mut h = Sha1::new();
    h.update(text.as_bytes());
    h.update((text.len() as u64).to_le_bytes().as_slice());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

fn load_cache(root: &Path) -> Result<EmbeddingCache> {
    let path = root.join(CACHE_FILE);
    if !path.exists() {
        return Ok(EmbeddingCache::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&raw)?)
}

fn save_cache(root: &Path, cache: &EmbeddingCache) -> Result<()> {
    let path = root.join(CACHE_FILE);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string(cache)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identifies_identical_vectors() {
        let v = vec![1.0, 2.0, 3.0];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-5);
        assert!(cosine(&v, &[-1.0, -2.0, -3.0]) + 1.0 < 1e-5);
        assert_eq!(cosine(&v, &[]), 0.0);
    }

    #[test]
    fn content_key_changes_with_length() {
        assert_ne!(content_key("ab"), content_key("abab"));
        assert_eq!(content_key("x"), content_key("x"));
    }
}
