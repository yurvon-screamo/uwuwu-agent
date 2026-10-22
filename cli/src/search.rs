use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use fs4::fs_std::FileExt;

use crate::cache::{Cache, ChunkRecord};
use crate::chunk::{read_meta, read_task_meta, split_chunks};
use crate::embed;

/// Advisory-лок на время работы с embeddings.db: SurrealDB однопроцессовый
/// (LOCK-файл), параллельный Cache::open падает. Сериализуем процессы CLI:
/// ждём до 120с (прогрев индекса бывает долгим), потом внятная ошибка.
fn acquire_db_lock(cache_path: &Path) -> Result<std::fs::File> {
    let lock_path = cache_path.with_extension("db.searchlock");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut warned = false;
    loop {
        match f.try_lock_exclusive() {
            Ok(true) => return Ok(f),
            Ok(false) | Err(_) => {
                if !warned {
                    eprintln!("  другой uwuwu-cli search держит индекс — ждём (до 120с)…");
                    warned = true;
                }
                if std::time::Instant::now() > deadline {
                    return Err(anyhow!(
                        "индекс занят другим uwuwu-cli search дольше 120с — повторите позже"
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        }
    }
}

const MODEL: &str = "qwen3-embedding:0.6b";
const THRESHOLD: f32 = 0.3;

#[derive(Clone)]
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

pub async fn search_with_filter(
    query: &str,
    doc_type: &str,
    wiki_root: &Path,
    cache_path: &Path,
    top_k: usize,
    filter: Option<&(dyn Fn(&Path) -> bool + Sync)>,
) -> Result<Vec<SearchResult>> {
    search_multi_with_filter(
        query,
        &[doc_type],
        wiki_root,
        cache_path,
        top_k,
        filter,
        None,
    )
    .await
}

/// Поиск по НЕСКОЛЬКИМ doc_type одним проходом (один Cache::open — SurrealDB
/// однопроцессовый: второй open в том же процессе ловит LOCK).
/// filepath результата содержит префикс doc_type (experience/…, facts/…).
/// threshold: минимальный score (None → константный THRESHOLD).
pub async fn search_multi_with_filter(
    query: &str,
    doc_types: &[&str],
    wiki_root: &Path,
    cache_path: &Path,
    top_k: usize,
    filter: Option<&(dyn Fn(&Path) -> bool + Sync)>,
    threshold: Option<f32>,
) -> Result<Vec<SearchResult>> {
    let base_dirs: Vec<std::path::PathBuf> = doc_types
        .iter()
        .map(|d| wiki_root.join(d))
        .collect::<Vec<_>>()
        .into_iter()
        .filter(|d| {
            if d.exists() {
                true
            } else {
                eprintln!("Error: directory '{}' not found", d.display());
                false
            }
        })
        .collect();
    if base_dirs.is_empty() {
        return Ok(vec![]);
    }

    let _db_lock = acquire_db_lock(cache_path)?;
    let cache = Cache::open(cache_path).await?;
    let cached = cache.load_all().await?;
    let cached_keys: Vec<String> = cached.keys().cloned().collect();

    let all_md_files: Vec<PathBuf> = base_dirs.iter().flat_map(|d| collect_md_files(d)).collect();
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

    // GC: записи кэша по исчезнувшим путям (миграции/переименования каталогов)
    // не удаляются сами — чистим в пределах текущих doc_type-префиксов.
    {
        let known: std::collections::HashSet<String> = md_files
            .iter()
            .map(|f| {
                f.strip_prefix(wiki_root)
                    .unwrap_or(f)
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        let stale_keys = cached_keys.clone();
        for stale in stale_keys {
            let in_scope = doc_types
                .iter()
                .any(|d| stale.starts_with(&format!("{d}/")));
            if in_scope && !known.contains(&stale) {
                let _ = cache.delete_file(&stale).await;
            }
        }
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
        .filter(|(_, score)| *score >= threshold.unwrap_or(THRESHOLD))
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

    // Graceful close ДО гашения tokio-рантайма: иначе surrealkv дропается
    // в shutdown и шумит «No runtime available» + JoinError::Cancelled.
    if let Err(e) = cache.close().await {
        eprintln!("warn: cache close: {e:#}");
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
