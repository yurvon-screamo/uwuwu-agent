//! Эмбеддинги: Gemini Embedding 2 через generativelanguage API (задача #1363,
//! решение владельца 19.11 — миграция с локальной Ollama; free tier принят
//! осознанно). Контракт (ai.google.dev/gemini-api/docs/embeddings):
//! batchEmbedContents с отдельным Content на каждый вход; task_type моделью
//! не поддерживается — асимметричный retrieval через текстовые префиксы
//! query/doc. Отказ маршрута — типизированная ошибка, не тихая слепота.

use async_trait::async_trait;
use uwuwu_usecase::{UsecaseError, UsecaseResult};

/// Модель эмбеддингов (stable, апрель 2026).
pub const EMBED_MODEL: &str = "gemini-embedding-2";
/// Размерность вектора: MRL-рекомендации 768/1536/3072, средний шаг —
/// половина дефолтных 3072; усечённые размеры модель нормализует сама.
pub const EMBED_MODEL_DIMS: usize = 1536;
/// Текстов в одном batchEmbedContents-вызове (спайк #1363: батчи по ~38
/// текстов ≈ 14–16k входных токенов проходят; кап 100 исторически).
pub const EMBED_BATCH: usize = 38;

const API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";
const SERVICE: &str = "gemini-embeddings";
/// Кап бэкоффа на 429 (RetryInfo может обещать и больше — не тише прод-тика).
pub const RATE_LIMIT_CAP_SECS: u64 = 15 * 60;

/// Источник векторов для семантического индекса. Реализации несут формат
/// префиксов провайдера сами — вызывающая сторона знает только (title, text).
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Векторы документов: пара (title, text) — doc-структура retrieval-контракта.
    async fn embed_docs(&self, docs: &[(String, String)]) -> UsecaseResult<Vec<Vec<f32>>>;
    /// Вектор поискового запроса (query-структура).
    async fn embed_query(&self, query: &str) -> UsecaseResult<Vec<f32>>;
}

/// Doc-структура asymmetric retrieval (доки Gemini Embedding 2): без тайтла —
/// явный `title: none`.
pub fn doc_text(title: &str, text: &str) -> String {
    let t = if title.is_empty() { "none" } else { title };
    format!("title: {t} | text: {text}")
}

/// Query-структура: задача поиска зашита префиксом (task_type не поддерживается).
pub fn query_text(query: &str) -> String {
    format!("task: search result | query: {query}")
}

/// RetryInfo из тела 429: `details[]` с `retryDelay` ("26s", "1m").
/// Возвращает секунды; без RetryInfo — None (вызывающая сторона ставит дефолт).
fn parse_retry_delay(body: &serde_json::Value) -> Option<u64> {
    let err = body.get("error")?;
    for d in err.get("details")?.as_array()? {
        let kind = d.get("@type")?.as_str()?;
        if !kind.ends_with("RetryInfo") {
            continue;
        }
        let raw = d.get("retryDelay")?.as_str()?;
        let secs = if raw.ends_with('s') {
            raw.trim_end_matches('s').parse::<u64>().ok()?
        } else if raw.ends_with('m') {
            raw.trim_end_matches('m').parse::<u64>().ok()? * 60
        } else {
            return None;
        };
        return Some(secs);
    }
    None
}

pub struct GeminiEmbeddings {
    api_key: String,
    model: String,
    http: reqwest::Client,
}

impl GeminiEmbeddings {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
        }
    }

    /// Батчи по EMBED_BATCH — последовательные вызовы; каждый батч = 1 запрос.
    async fn embed_many(&self, texts: Vec<String>) -> UsecaseResult<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(EMBED_BATCH) {
            out.extend(self.embed_one(chunk).await?);
        }
        Ok(out)
    }

    async fn embed_one(&self, texts: &[String]) -> UsecaseResult<Vec<Vec<f32>>> {
        let requests: Vec<serde_json::Value> = texts
            .iter()
            .map(|t| {
                serde_json::json!({
                    "model": format!("models/{}", self.model),
                    "content": { "parts": [{ "text": t }] },
                    "outputDimensionality": EMBED_MODEL_DIMS,
                })
            })
            .collect();
        let resp = self
            .http
            .post(format!("{API_BASE}/models/{}:batchEmbedContents", self.model))
            .header("x-goog-api-key", &self.api_key)
            .json(&serde_json::json!({ "requests": requests }))
            .send()
            .await
            .map_err(|e| UsecaseError::State(format!("{SERVICE} transport: {e}")))?;
        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| UsecaseError::State(format!("{SERVICE} body: {e}")))?;
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(UsecaseError::RateLimited {
                service: SERVICE.into(),
                retry_after_secs: parse_retry_delay(&body).or(Some(300)),
            });
        }
        if !status.is_success() {
            return Err(UsecaseError::State(format!(
                "{SERVICE} HTTP {status}: {}",
                body.to_string().chars().take(160).collect::<String>()
            )));
        }
        let embs = body
            .get("embeddings")
            .and_then(|v| v.as_array())
            .ok_or_else(|| UsecaseError::State(format!("{SERVICE}: нет embeddings в ответе")))?;
        if embs.len() != texts.len() {
            return Err(UsecaseError::State(format!(
                "{SERVICE}: ждали {} векторов, получили {}",
                texts.len(),
                embs.len()
            )));
        }
        let vecs: Vec<Vec<f32>> = embs
            .iter()
            .map(|e| {
                e.get("values")
                    .and_then(|v| v.as_array())
                    .map(|v| {
                        v.iter()
                            .filter_map(|x| x.as_f64().map(|f| f as f32))
                            .collect::<Vec<f32>>()
                    })
                    .ok_or_else(|| UsecaseError::State(format!("{SERVICE}: пустой embedding")))
            })
            .collect::<UsecaseResult<Vec<_>>>()?;
        if vecs.iter().any(|v| v.is_empty()) {
            return Err(UsecaseError::State(format!("{SERVICE}: пустой embedding")));
        }
        Ok(vecs)
    }
}

#[async_trait]
impl Embedder for GeminiEmbeddings {
    async fn embed_docs(&self, docs: &[(String, String)]) -> UsecaseResult<Vec<Vec<f32>>> {
        let texts = docs.iter().map(|(t, c)| doc_text(t, c)).collect();
        self.embed_many(texts).await
    }

    async fn embed_query(&self, query: &str) -> UsecaseResult<Vec<f32>> {
        let mut v = self
            .embed_many(vec![query_text(query)])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| UsecaseError::State(format!("{SERVICE}: пустой ответ на запрос")))?;
        if v.len() != EMBED_MODEL_DIMS {
            return Err(UsecaseError::State(format!(
                "{SERVICE}: вектор {}-dim вместо {EMBED_MODEL_DIMS}",
                v.len()
            )));
        }
        Ok(std::mem::take(&mut v))
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
    fn retry_delay_parsed_from_retry_info() {
        let body = serde_json::json!({
            "error": {
                "code": 429,
                "message": "quota exceeded",
                "details": [
                    {"@type": "type.googleapis.com/google.rpc.QuotaFailure", "violations": []},
                    {"@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "26s"}
                ]
            }
        });
        assert_eq!(parse_retry_delay(&body), Some(26));
    }

    #[test]
    fn retry_delay_minutes_and_absent() {
        let mins = serde_json::json!({
            "error": {"details": [{"@type": "google.rpc.RetryInfo", "retryDelay": "2m"}]}
        });
        assert_eq!(parse_retry_delay(&mins), Some(120));
        let none = serde_json::json!({"error": {"message": "no details"}});
        assert_eq!(parse_retry_delay(&none), None);
    }

    #[test]
    fn chunking_splits_long_docs() {
        let md = "абзац\n\n".repeat(400);
        let chunks = chunk_text(&md);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.len() <= 1400));
    }
}
