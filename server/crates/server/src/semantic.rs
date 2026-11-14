//! Семантика: EmbeddingClient (OpenRouter free-модель, батчи) и
//! SemanticStore (плоский векторный индекс + полнотекст по чанкам).
//! Отклонение от решения «LanceDB» задокументировано в теле #1133
//! (трейт SemanticBackend — движок заменяем).

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

    /// Эмбеддинги батча входов (2048-dim каждый); порядок ответа
    /// верифицируется по index-полю (защита от склейки не по порядку).
    pub async fn embed_batch(&self, inputs: &[String]) -> UsecaseResult<Vec<Vec<f32>>> {
        let mut out: Vec<Vec<f32>> = Vec::with_capacity(inputs.len());
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
            let arr = body["data"].as_array().cloned().unwrap_or_default();
            if arr.len() != chunk.len() {
                return Err(UsecaseError::State(format!(
                    "embeddings: ждали {} векторов, пришло {}",
                    chunk.len(),
                    arr.len()
                )));
            }
            // порядок по index-полю ответа
            let mut ordered: Vec<Option<Vec<f32>>> = (0..chunk.len()).map(|_| None).collect();
            for d in &arr {
                let idx = d["index"].as_u64().unwrap_or(u64::MAX) as usize;
                let vec: Vec<f32> = d["embedding"]
                    .as_array()
                    .map(|v| {
                        v.iter()
                            .filter_map(|x| x.as_f64().map(|f| f as f32))
                            .collect()
                    })
                    .unwrap_or_default();
                if idx >= ordered.len() || vec.is_empty() {
                    return Err(UsecaseError::State("embeddings: битый index/вектор".into()));
                }
                ordered[idx] = Some(vec);
            }
            for v in ordered {
                out.push(v.ok_or_else(|| {
                    UsecaseError::State("embeddings: пропуск в порядке векторов".into())
                })?);
            }
        }
        Ok(out)
    }
}

/// Чанк дока: текст (полнотекст) + вектор (семантика) + хэш дока (инкремент).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    /// Слаг дока (без #idx — выдача поиска читается docs_read).
    pub slug: String,
    pub idx: u32,
    /// Хэш всего дока на момент индексации (инкрементальность).
    pub doc_hash: u64,
    /// Полный текст чанка (полнотекст по контенту).
    pub text: String,
    #[serde(default)]
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlatIndex {
    /// slug → чанки (целиком замещаются при реиндексации дока).
    pub docs: HashMap<String, Vec<Chunk>>,
}

impl FlatIndex {
    /// Топ-n по cosine.
    pub fn search(&self, query: &[f32], top: usize) -> Vec<(f32, String)> {
        let mut best: HashMap<String, f32> = HashMap::new();
        for chunks in self.docs.values() {
            for c in chunks {
                if c.vector.is_empty() {
                    continue;
                }
                let s = cosine(query, &c.vector);
                let e = best.entry(c.slug.clone()).or_insert(f32::MIN);
                if s > *e {
                    *e = s;
                }
            }
        }
        let mut scores: Vec<(f32, String)> = best.into_iter().map(|(s, v)| (v, s)).collect();
        scores.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(top);
        scores
    }

    /// Полнотекст: все токены (lowercase, >1 символа) в тексте чанков дока.
    pub fn fulltext(&self, tokens: &[String], top: usize) -> Vec<String> {
        let mut hits = Vec::new();
        'doc: for (slug, chunks) in &self.docs {
            for t in tokens {
                let needle = t.to_lowercase();
                let found = chunks
                    .iter()
                    .any(|c| c.text.to_lowercase().contains(&needle));
                if !found {
                    continue 'doc;
                }
            }
            hits.push(slug.clone());
            if hits.len() >= top {
                break;
            }
        }
        hits
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

/// Персистентный плоский индекс (json, атомарная запись).
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

    /// Текущий хэш дока в индексе (по первому чанку).
    pub fn doc_hash(&self, slug: &str) -> Option<u64> {
        self.index
            .lock()
            .expect("index lock")
            .docs
            .get(slug)
            .and_then(|cs| cs.first().map(|c| c.doc_hash))
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

/// Чанковка дока: абзацы группами ~1200 символов.
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
    fn flat_index_search_doc_slug() {
        let mut idx = FlatIndex::default();
        idx.docs.insert(
            "a".into(),
            vec![Chunk {
                slug: "a".into(),
                idx: 0,
                doc_hash: 1,
                text: "x".into(),
                vector: vec![1.0, 0.0],
            }],
        );
        idx.docs.insert(
            "b".into(),
            vec![Chunk {
                slug: "b".into(),
                idx: 0,
                doc_hash: 2,
                text: "y".into(),
                vector: vec![0.0, 1.0],
            }],
        );
        let hits = idx.search(&[1.0, 0.1], 1);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1, "a", "слог дока без #idx");
    }

    #[test]
    fn fulltext_and_tokens() {
        let mut idx = FlatIndex::default();
        idx.docs.insert(
            "rust-опыт".into(),
            vec![Chunk {
                slug: "rust-опыт".into(),
                idx: 0,
                doc_hash: 1,
                text: "Канон: cargo clippy -D warnings в CI".into(),
                vector: vec![],
            }],
        );
        idx.docs.insert(
            "other".into(),
            vec![Chunk {
                slug: "other".into(),
                idx: 0,
                doc_hash: 2,
                text: "мусор".into(),
                vector: vec![],
            }],
        );
        assert_eq!(
            idx.fulltext(&["clippy".into(), "cargo".into()], 5),
            vec!["rust-опыт".to_string()]
        );
        assert!(idx
            .fulltext(&["clippy".into(), "нетам".into()], 5)
            .is_empty());
    }

    #[test]
    fn chunking_splits_long_docs() {
        let md = "абзац\n\n".repeat(400);
        let chunks = chunk_text(&md);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.len() <= 1400));
    }
}
