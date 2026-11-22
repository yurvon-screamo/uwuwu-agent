//! Чтение семантического индекса (#1473): гибрид векторного поиска и
//! BM25-полнотекста по FTS-индексу «текст». Write-путь (sync/реиндекс) —
//! в semantic_backend.rs; здесь только запросы.

use lancedb::arrow::arrow_array::{Array, Float32Array, LargeStringArray};
use lancedb::index::scalar::FullTextSearchQuery;
use lancedb::query::{ExecutableQuery, QueryBase};
use std::collections::HashMap;

use super::semantic_backend::{err, LanceSemanticBackend};
use uwuwu_usecase::UsecaseResult;

/// Векторный топ-k по запросу: эмбеддинг query, ANN по чанкам, лучший
/// слаг по сходству. Мясо трейта SemanticBackend — импл в semantic_backend.
pub(super) async fn vector_search(
    backend: &LanceSemanticBackend,
    query: &str,
    top: usize,
) -> UsecaseResult<Vec<(f32, String)>> {
    let qv = backend.embeddings.embed_query(query).await?;
    let vq = backend
        .chunks()
        .await?
        .vector_search(qv.clone())
        .map_err(|e| err(format!("vector build: {e}")))?;
    let stream = vq
        .limit(top * 3)
        .execute()
        .await
        .map_err(|e| err(format!("vector: {e}")))?;
    use futures::stream::StreamExt;
    let mut stream = stream;
    let mut best: HashMap<String, f32> = HashMap::new();
    while let Some(batch) = stream.next().await {
        let batch = batch.map_err(|e| err(format!("vstream: {e}")))?;
        let slugs = batch
            .column_by_name("slug")
            .and_then(|c| c.as_any().downcast_ref::<LargeStringArray>());
        let dists = batch
            .column_by_name("_distance")
            .and_then(|c| c.as_any().downcast_ref::<Float32Array>());
        if let (Some(s), Some(d)) = (slugs, dists) {
            for i in 0..s.len() {
                let similarity = 1.0 - d.value(i);
                let e = best.entry(s.value(i).to_string()).or_insert(f32::MIN);
                if similarity > *e {
                    *e = similarity;
                }
            }
        }
    }
    let mut out: Vec<(f32, String)> = best.into_iter().map(|(k, v)| (v, k)).collect();
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(top);
    Ok(out)
}

/// BM25-полнотекст: топ-k слагов по токенам (AND-семантика). Отказ FTS
/// не молчит (#1473): Err пробрасывается в usecase — рендер «— недоступен»
/// в секции полнотекста, причина видна в выдаче.
pub(super) async fn fulltext(
    backend: &LanceSemanticBackend,
    tokens: &[String],
    top: usize,
) -> UsecaseResult<Vec<String>> {
    let table = backend.chunks().await?;
    let mut per_token: Vec<std::collections::HashSet<String>> = Vec::new();
    for token in tokens {
        per_token.push(token_hits(&table, token, top).await?);
    }
    let mut it = per_token.into_iter();
    let Some(first) = it.next() else {
        return Ok(Vec::new());
    };
    let mut inter: Vec<String> = first.into_iter().collect();
    for set in it {
        inter.retain(|s| set.contains(s));
        if inter.is_empty() {
            break;
        }
    }
    inter.truncate(top);
    Ok(inter)
}

/// Слаги чанков, совпавших с токеном (BM25 по FTS-индексу «текст»).
async fn token_hits(
    table: &lancedb::table::Table,
    token: &str,
    top: usize,
) -> UsecaseResult<std::collections::HashSet<String>> {
    let q = FullTextSearchQuery::new(token.to_string());
    let stream = table
        .query()
        .full_text_search(q)
        .limit(top * 3)
        .execute()
        .await
        .map_err(|e| {
            tracing::warn!(event = "semantic.fts_unavailable", error = %e, "FTS недоступен");
            err(format!("fts: {e}"))
        })?;
    use futures::stream::StreamExt;
    let mut stream = stream;
    let mut hits = std::collections::HashSet::new();
    while let Some(batch) = stream.next().await {
        let batch = batch.map_err(|e| err(format!("fts stream: {e}")))?;
        if let Some(col) = batch.column_by_name("slug") {
            if let Some(arr) = col.as_any().downcast_ref::<LargeStringArray>() {
                for i in 0..arr.len() {
                    hits.insert(arr.value(i).to_string());
                }
            }
        }
    }
    Ok(hits)
}
