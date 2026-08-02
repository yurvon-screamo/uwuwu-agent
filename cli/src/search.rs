use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::cache::{Cache, ChunkRecord};
use crate::chunk::{read_meta, read_task_meta, split_chunks};
use crate::embed;

const MODEL: &str = "qwen3-embedding:0.6b";
const THRESHOLD: f32 = 0.3;

pub struct SearchResult {
    pub filepath: String,
    pub score: f32,
    pub title: Option<String>,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub deadline: Option<String>,
    pub designed: Option<bool>,
}

pub async fn search(
    query: &str,
    doc_type: &str,
    wiki_root: &Path,
    cache_path: &Path,
    top_k: usize,
) -> Result<Vec<SearchResult>> {
    search_with_filter(query, doc_type, wiki_root, cache_path, top_k, None).await
}

pub async fn reindex_file(exp_relpath: &str, wiki_root: &Path, cache_path: &Path) -> Result<()> {
    let full_path = wiki_root.join("experience").join(exp_relpath);
    if !full_path.exists() {
        anyhow::bail!("file not found: {}", full_path.display());
    }

    let cache_key = format!("experience/{exp_relpath}");
    let mtime = file_mtime(&full_path);
    let chunks = split_chunks(&full_path, wiki_root);
    if chunks.is_empty() {
        anyhow::bail!("no chunks produced for {cache_key} (empty body?)");
    }

    let cache = Cache::open(cache_path).await?;
    cache.delete_file(&cache_key).await?;

    let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
    let previews: Vec<String> = chunks
        .iter()
        .map(|c| {
            c.text
                .chars()
                .take(200)
                .collect::<String>()
                .replace('\n', " ")
        })
        .collect();

    const BATCH: usize = 20;
    let mut idx = 0i64;
    for batch in texts.chunks(BATCH) {
        let embeddings = embed::batch_embed(MODEL, batch).await?;
        for emb in embeddings {
            let preview = &previews[idx as usize];
            let rec = ChunkRecord {
                filepath: cache_key.clone(),
                chunk_index: idx,
                mtime,
                embedding: emb.clone(),
                preview: preview.clone(),
            };
            cache.put_chunk(&rec).await?;
            idx += 1;
        }
    }

    Ok(())
}

pub async fn search_with_filter(
    query: &str,
    doc_type: &str,
    wiki_root: &Path,
    cache_path: &Path,
    top_k: usize,
    filter: Option<&(dyn Fn(&Path) -> bool + Sync)>,
) -> Result<Vec<SearchResult>> {
    let base_dir = wiki_root.join(doc_type);
    if !base_dir.exists() {
        eprintln!("Error: directory '{}' not found", base_dir.display());
        return Ok(vec![]);
    }

    let cache = Cache::open(cache_path).await?;
    let cached = cache.load_all().await?;

    let all_md_files = collect_md_files(&base_dir);
    let md_files = match filter {
        Some(f) => all_md_files.into_iter().filter(|p| f(p)).collect(),
        None => all_md_files,
    };
    let mut all_chunks: Vec<(String, Vec<f32>)> = Vec::new();
    let mut pending: Vec<(String, i64, Vec<String>, Vec<String>)> = Vec::new();
    let mut need_embed = 0u32;

    for f in &md_files {
        let rel = f
            .strip_prefix(wiki_root)
            .unwrap_or(f)
            .to_string_lossy()
            .to_string();
        let mtime = file_mtime(f);
        let chunks = split_chunks(f, wiki_root);

        if let Some((cached_mtime, records)) = cached.get(&rel) {
            if *cached_mtime == mtime && records.len() == chunks.len() {
                for rec in records {
                    all_chunks.push((rel.clone(), rec.embedding.clone()));
                }
                continue;
            }
        }

        let _ = cache.delete_file(&rel).await;

        let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
        let previews: Vec<String> = chunks
            .iter()
            .map(|c| {
                c.text
                    .chars()
                    .take(200)
                    .collect::<String>()
                    .replace('\n', " ")
            })
            .collect();

        pending.push((rel.clone(), mtime, texts, previews));
    }

    for (rel, mtime, texts, previews) in &pending {
        const BATCH: usize = 20;
        let mut idx = 0;

        for batch in texts.chunks(BATCH) {
            let embeddings = embed::batch_embed(MODEL, batch).await?;
            for emb in embeddings {
                let preview = &previews[idx];

                let rec = ChunkRecord {
                    filepath: rel.clone(),
                    chunk_index: idx as i64,
                    mtime: *mtime,
                    embedding: emb.clone(),
                    preview: preview.clone(),
                };
                cache.put_chunk(&rec).await?;

                all_chunks.push((rel.clone(), emb));
                need_embed += 1;
                idx += 1;
            }
        }
    }

    if need_embed > 0 {
        eprintln!(
            "  embedded {need_embed} chunks across {} articles",
            md_files.len()
        );
    }

    let q_emb = embed::embed(MODEL, query).await?;

    let mut article_scores: HashMap<String, f32> = HashMap::new();

    for (rel, emb) in &all_chunks {
        let sim = cosine(&q_emb, emb);
        article_scores
            .entry(rel.clone())
            .and_modify(|best| {
                if sim > *best {
                    *best = sim;
                }
            })
            .or_insert(sim);
    }

    let mut results: Vec<(String, f32)> = article_scores
        .into_iter()
        .filter(|(_, score)| *score >= THRESHOLD)
        .collect();

    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let top: Vec<(String, f32)> = results.into_iter().take(top_k).collect();

    let mut search_results = Vec::new();
    for (rel, score) in top {
        let full_path = wiki_root.join(&rel);
        let (title, description) = read_meta(&full_path);
        let meta = read_task_meta(&full_path);

        search_results.push(SearchResult {
            filepath: rel,
            score,
            title,
            description,
            priority: meta.priority,
            deadline: meta.deadline,
            designed: meta.designed,
        });
    }

    Ok(search_results)
}

fn collect_md_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_md_recursive(dir, &mut files);
    files.sort();
    files
}

fn collect_md_recursive(dir: &Path, files: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_md_recursive(&path, files);
            } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                files.push(path);
            }
        }
    }
}

fn file_mtime(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}
