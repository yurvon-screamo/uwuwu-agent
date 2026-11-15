//! Эмбеддинги: Ollama-совместимый /api/embeddings (локальная машина через
//! туннель relay; решения #1133: LanceDB embedded, free-квоты не участвуют).

use uwuwu_usecase::error::{UsecaseError, UsecaseResult};

/// Модель эмбеддингов локальной Ollama.
pub const EMBED_MODEL: &str = "qwen3-embedding:0.6b";
/// Размерность вектора модели.
pub const EMBED_MODEL_DIMS: usize = 1024;
/// Плечо батча (последовательные вызовы, локальный туннель терпит).
pub const EMBED_BATCH: usize = 16;

pub struct OllamaEmbeddings {
    url: String,
    pub model: String,
    http: reqwest::Client,
}

impl OllamaEmbeddings {
    pub fn new(url: String, model: String) -> Self {
        Self {
            url: url.trim_end_matches('/').to_string(),
            model,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("reqwest client"),
        }
    }

    /// Эмбеддинги входов (последовательные вызовы /api/embeddings).
    pub async fn embed_batch(&self, inputs: &[String]) -> UsecaseResult<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(inputs.len());
        for input in inputs.iter().take(EMBED_BATCH.max(inputs.len())) {
            let resp = self
                .http
                .post(format!("{}/api/embeddings", self.url))
                .json(&serde_json::json!({ "model": self.model, "prompt": input }))
                .send()
                .await
                .map_err(|e| UsecaseError::State(format!("ollama transport: {e}")))?;
            let status = resp.status();
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| UsecaseError::State(format!("ollama body: {e}")))?;
            if !status.is_success() {
                return Err(UsecaseError::State(format!(
                    "ollama HTTP {status}: {}",
                    body.to_string().chars().take(160).collect::<String>()
                )));
            }
            let vec: Vec<f32> = body["embedding"]
                .as_array()
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_f64().map(|f| f as f32))
                        .collect()
                })
                .unwrap_or_default();
            if vec.is_empty() {
                return Err(UsecaseError::State("ollama: пустой embedding".into()));
            }
            out.push(vec);
        }
        Ok(out)
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
    fn url_trimmed() {
        let e = OllamaEmbeddings::new("http://x:1/".into(), "m".into());
        assert_eq!(e.url, "http://x:1");
    }

    #[test]
    fn chunking_splits_long_docs() {
        let md = "абзац\n\n".repeat(400);
        let chunks = chunk_text(&md);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.len() <= 1400));
    }
}
