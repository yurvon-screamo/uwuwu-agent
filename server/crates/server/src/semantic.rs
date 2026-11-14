//! Семантика: EmbeddingClient (OpenRouter free-модель, батчи) и
//! SemanticStore (плоский векторный индекс на диске, cosine в RAM).
//! Движок заменяем трейтом (решение #1133: LanceDB pre-1.0 — изоляция);
//! текущая реализация: чанки × f32-векторы в одном файле, корпус вики
//! в RAM-бюджете (десятки MB на тысячи чанков).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use uwuwu_usecase::error::{UsecaseError, UsecaseResult};

pub const EMBED_MODEL: &str = "nvidia/nemotron-3.5-embed-1b:free";
/// Батч входов на запрос (проверено живьём: 32 стабильно).
pub const EMBED_BATCH: usize = 32;

pub struct OpenRouterEmbeddings {
    key: String,
    http: reqwest::Client,
    /// Фоллбэк не строим (решение владельца: только локальная Ollama —
    /// разовые туннели операционкой; постоянного механизма нет).
    pub model: String,
}

impl OpenRouterEmbeddings {
    pub fn new(key: String) -> Self {
        Self {
            key,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("reqwest client"),
            model: EMBED_MODEL.into(),
        }
    }

    /// Эмбеддинги батча входов (2048-dim каждый).
    pub async fn embed_batch(&self, inputs: &[String]) -> UsecaseResult<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(inputs.len());
        for chunk in inputs.chunks(EMBED_BATCH) {
            let resp = self
                .http
                .post("https://openrouter.ai/api/v1/embeddings")
                .bearer_auth(&self.key)
                .json(&serde_json::json!({ "model": self.model, "input": chunk }))
                .send()
                .await
                .map_err(|e| UsecaseError::State(format!("embeddings transport: {e}")))?;
            let status = resp.status();
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| UsecaseError::State(format!("embeddings body: {e}")))?;
            if !status.is_success() {
                return Err(UsecaseError::State(format!(
                    "embeddings HTTP {status}: {}",
                    body.to_string().chars().take(160).collect::<String>()
                )));
            }
            let mut vecs: Vec<Vec<f32>> = body["data"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|d| {
                            d["embedding"].as_array().map(|v| {
                                v.iter()
                                    .filter_map(|x| x.as_f64().map(|f| f as f32))
                                    .collect()
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            if vecs.len() != chunk.len() {
                return Err(UsecaseError::State(format!(
                    "embeddings: ждали {} векторов, пришло {}",
                    chunk.len(),
                    vecs.len()
                )));
            }
            vecs.sort_by_key(|_| 0);
            out.append(&mut vecs);
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub slug: String,
    /// Порядковый номер чанка дока.
    pub idx: u32,
    pub text_head: String,
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlatIndex {
    /// slug → chunks.
    pub docs: HashMap<String, Vec<Chunk>>,
}

impl FlatIndex {
    /// Топ-n по cosine.
    pub fn search(&self, query: &[f32], top: usize) -> Vec<(f32, String, u32)> {
        let mut scores: Vec<(f32, String, u32)> = Vec::new();
        for chunks in self.docs.values() {
            for c in chunks {
                let s = cosine(query, &c.vector);
                scores.push((s, c.slug.clone(), c.idx));
            }
        }
        scores.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(top);
        scores
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for i in 0..n {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Персистентный плоский индекс: один bincode-подобный файл (json-строки
/// честны и отлаживаемы; объём десятки MB — приемлемо).
pub struct FileSemanticStore {
    path: PathBuf,
    pub index: std::sync::Mutex<FlatIndex>,
}

impl FileSemanticStore {
    pub fn load(path: PathBuf) -> Self {
        let index = std::fs::read(&path)
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default();
        Self {
            path,
            index: std::sync::Mutex::new(index),
        }
    }

    pub fn save(&self) -> UsecaseResult<()> {
        let raw = serde_json::to_vec(&*self.index.lock().expect("index lock"))
            .map_err(|e| UsecaseError::State(format!("index сериализация: {e}")))?;
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, &raw).map_err(|e| UsecaseError::State(format!("index write: {e}")))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| UsecaseError::State(format!("index rename: {e}")))?;
        Ok(())
    }

    /// Хэш текущего содержимого дока для инкрементальности.
    pub fn doc_hash(&self, slug: &str) -> Option<u64> {
        self.index
            .lock()
            .expect("index lock")
            .docs
            .get(slug)
            .map(|cs| cs.first().map(|c| text_hash(&c.text_head)).unwrap_or(0))
    }
}

/// Простой стабильный хэш (FNV-1a) без зависимостей.
pub fn text_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Чанковка дока: полные чанки (текст) для эмбеддинга, ~1200 символов.
pub fn chunk_text(md: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for para in md.split("\n\n") {
        if buf.len() + para.len() > 1200 && !buf.is_empty() {
            out.push(std::mem::take(&mut buf));
        }
        if !buf.is_empty() {
            buf.push_str("\n\n");
        }
        buf.push_str(para);
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_orders_similar_vectors() {
        let q = vec![1.0, 0.0, 0.0];
        let a = vec![0.9, 0.1, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!(cosine(&q, &a) > cosine(&q, &b));
        assert_eq!(cosine(&q, &q), 1.0);
    }

    #[test]
    fn flat_index_search_top() {
        let mut idx = FlatIndex::default();
        idx.docs.insert(
            "a".into(),
            vec![Chunk {
                slug: "a".into(),
                idx: 0,
                text_head: "x".into(),
                vector: vec![1.0, 0.0],
            }],
        );
        idx.docs.insert(
            "b".into(),
            vec![Chunk {
                slug: "b".into(),
                idx: 0,
                text_head: "y".into(),
                vector: vec![0.0, 1.0],
            }],
        );
        let hits = idx.search(&[1.0, 0.1], 1);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1, "a");
    }

    #[test]
    fn chunking_splits_long_docs() {
        let md = "абзац\n\n".repeat(400);
        let chunks = chunk_text(&md);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.len() <= 1400));
    }
}
