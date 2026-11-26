//! Эмбеддинги: локальная Ollama embeddinggemma-2 через /api/embed (#1687,
//! слово владельца 26.11 — возврат с локального Gemini free tier).
//! Контракт модели (ollama.com/library/embeddinggemma-2): 768-dim,
//! task-steered префиксы doc/query совпадают с прежним клиентом, MRL-усечение
//! через options не применяется API. Отказ маршрута — типизированная ошибка.

use async_trait::async_trait;
use std::time::Duration;
use uwuwu_usecase::{UsecaseError, UsecaseResult};

/// Модель с явным тегом: голого `embeddinggemma-2` в хранилище нет.
pub const EMBED_MODEL: &str = "embeddinggemma-2:740m";
/// Нативная размерность модели (подтверждена живым /api/embed).
pub const EMBED_MODEL_DIMS: usize = 768;
/// Текстов в одном /api/embed-вызове (живой замер: 1,38 с на батч).
pub const EMBED_BATCH: usize = 38;
const SERVICE: &str = "ollama-embeddings";

/// Таймауты клиента: connect короткий, total — по классу вызова. Батч 60 с
/// укладывается в гард тика 120 с; query 15 с не держит поиск в стейл-туннеле.
#[derive(Debug, Clone, Copy)]
pub struct ClientTimeouts {
    pub connect: Duration,
    pub query_total: Duration,
    pub batch_total: Duration,
}

impl Default for ClientTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(2),
            query_total: Duration::from_secs(15),
            batch_total: Duration::from_secs(60),
        }
    }
}

/// Источник векторов для семантического индекса. Реализации несут формат
/// префиксов провайдера сами — вызывающая сторона знает только (title, text).
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Векторы документов: пара (title, text) — doc-структура retrieval-контракта.
    async fn embed_docs(&self, docs: &[(String, String)]) -> UsecaseResult<Vec<Vec<f32>>>;
    /// Вектор поискового запроса (query-структура).
    async fn embed_query(&self, query: &str) -> UsecaseResult<Vec<f32>>;
}

/// Doc-структура asymmetric retrieval (EmbeddingGemma 2): без тайтла — `none`.
pub fn doc_text(title: &str, text: &str) -> String {
    let t = if title.is_empty() { "none" } else { title };
    format!("title: {t} | text: {text}")
}

/// Query-структура: задача поиска зашита префиксом (task-steered модель).
pub fn query_text(query: &str) -> String {
    format!("task: search result | query: {query}")
}

pub struct OllamaEmbeddings {
    base_url: String,
    model: String,
    http: reqwest::Client,
    timeouts: ClientTimeouts,
}

impl OllamaEmbeddings {
    pub fn new(base_url: String, model: String) -> Self {
        Self::with_timeouts(base_url, model, ClientTimeouts::default())
    }

    /// Шов для тестов: короткие таймауты вместо production-значений.
    pub fn with_timeouts(base_url: String, model: String, timeouts: ClientTimeouts) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(timeouts.connect)
            .build()
            .expect("reqwest client");
        Self {
            base_url,
            model,
            http,
            timeouts,
        }
    }

    async fn embed_many(
        &self,
        texts: Vec<String>,
        total: Duration,
    ) -> UsecaseResult<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(EMBED_BATCH) {
            out.extend(self.embed_one(chunk, total).await?);
        }
        Ok(out)
    }

    async fn embed_one(&self, texts: &[String], total: Duration) -> UsecaseResult<Vec<Vec<f32>>> {
        let resp = tokio::time::timeout(
            total,
            self.http
                .post(format!("{}/api/embed", self.base_url.trim_end_matches('/')))
                .json(&serde_json::json!({ "model": self.model, "input": texts }))
                .send(),
        )
        .await
        .map_err(|_| UsecaseError::State(format!("{SERVICE}: request timeout after {total:?}")))?
        .map_err(|e| UsecaseError::State(format!("{SERVICE} transport: {e}")))?;
        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| UsecaseError::State(format!("{SERVICE} body: {e}")))?;
        if !status.is_success() {
            return Err(UsecaseError::State(format!(
                "{SERVICE} HTTP {status}: {}",
                body.to_string().chars().take(160).collect::<String>()
            )));
        }
        let embs = body
            .get("embeddings")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                UsecaseError::State(format!("{SERVICE}: no embeddings in the response"))
            })?;
        if embs.len() != texts.len() {
            return Err(UsecaseError::State(format!(
                "{SERVICE}: expected {} vectors, got {}",
                texts.len(),
                embs.len()
            )));
        }
        let vecs: Vec<Vec<f32>> = embs
            .iter()
            .map(|e| {
                e.as_array()
                    .map(|v| {
                        v.iter()
                            .filter_map(|x| x.as_f64().map(|f| f as f32))
                            .collect()
                    })
                    .ok_or_else(|| UsecaseError::State(format!("{SERVICE}: empty embedding")))
            })
            .collect::<UsecaseResult<Vec<_>>>()?;
        if let Some(v) = vecs.iter().find(|v| v.len() != EMBED_MODEL_DIMS) {
            return Err(UsecaseError::State(format!(
                "{SERVICE}: a {}-dim vector instead of {EMBED_MODEL_DIMS}",
                v.len()
            )));
        }
        Ok(vecs)
    }
}

#[async_trait]
impl Embedder for OllamaEmbeddings {
    async fn embed_docs(&self, docs: &[(String, String)]) -> UsecaseResult<Vec<Vec<f32>>> {
        let texts = docs.iter().map(|(t, c)| doc_text(t, c)).collect();
        self.embed_many(texts, self.timeouts.batch_total).await
    }

    async fn embed_query(&self, query: &str) -> UsecaseResult<Vec<f32>> {
        self.embed_many(vec![query_text(query)], self.timeouts.query_total)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| UsecaseError::State(format!("{SERVICE}: empty response to the request")))
    }
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
    fn doc_prefix_formats_title() {
        assert_eq!(
            doc_text("Projects/x", "тело"),
            "title: Projects/x | text: тело"
        );
        assert_eq!(doc_text("", "тело"), "title: none | text: тело");
    }

    #[test]
    fn query_prefix_formats_task() {
        assert_eq!(
            query_text("лимиты крона"),
            "task: search result | query: лимиты крона"
        );
    }

    #[test]
    fn chunking_splits_long_docs() {
        let md = "абзац\n\n".repeat(400);
        let chunks = chunk_text(&md);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.len() <= 1400));
    }
}
