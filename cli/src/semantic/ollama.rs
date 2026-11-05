//! Клиент Ollama `/api/embed`: батч-эмбеддинги с классификацией отказов
//! маршрута (semantic-половина поиска деградирует по классу, а не падает).
//!
//! Контракт ответа: `{"embeddings": [[f32; dim], …]}` (docs.ollama.com/api,
//! эндпоинт /api/embed; живая проба 05.11 — qwen3-embedding:0.6b, 1024-dim).

use serde_json::Value;
use std::time::Duration;

#[derive(Debug)]
pub enum EmbedError {
    /// Транспорт: сервер не отвечает (лежит/не запущен/таймаут).
    Unreachable { url: String, cause: String },
    /// 404 «model not found» — модель не вытянута.
    ModelMissing { model: String },
    /// Прочий неуспешный ответ API.
    Api { status: String, body: String },
    /// Размерность ответа ≠ размерности индекса (смена модели/варианта).
    DimMismatch { index_dim: usize, got_dim: usize },
}

impl std::fmt::Display for EmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable { url, cause } => {
                write!(f, "Ollama недоступна ({url}): {cause}")
            }
            Self::ModelMissing { model } => {
                write!(f, "модели «{model}» нет — ollama pull {model}")
            }
            Self::Api { status, body } => write!(f, "Ollama API {status}: {body}"),
            Self::DimMismatch { index_dim, got_dim } => write!(
                f,
                "размерность ответа {got_dim} ≠ индекса {index_dim} — uwuwu-cli semantic rebuild"
            ),
        }
    }
}

impl std::error::Error for EmbedError {}

pub struct OllamaClient {
    base: String,
    model: String,
    client: reqwest::Client,
}

impl OllamaClient {
    pub fn new(base: &str, model: &str, timeout: Duration) -> anyhow::Result<Self> {
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            model: model.to_string(),
            client: reqwest::Client::builder().timeout(timeout).build()?,
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Батч-эмбеддинг; `index_dim = Some(d)` включает dim-guard (первый ответ
    /// свежего индекса принимает фактическую dim как канон).
    pub async fn embed(
        &self,
        inputs: &[String],
        index_dim: Option<usize>,
    ) -> Result<Vec<Vec<f32>>, EmbedError> {
        let url = format!("{}/api/embed", self.base);
        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({ "model": self.model, "input": inputs }))
            .send()
            .await
            .map_err(|e| EmbedError::Unreachable {
                url: url.clone(),
                cause: e.to_string(),
            })?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if status.as_u16() == 404 && body.contains("not found") {
                return Err(EmbedError::ModelMissing {
                    model: self.model.clone(),
                });
            }
            return Err(EmbedError::Api {
                status: status.to_string(),
                body: truncate(&body, 200),
            });
        }

        let payload: Value = resp.json().await.map_err(|e| EmbedError::Unreachable {
            url: url.clone(),
            cause: format!("битый JSON ответа: {e}"),
        })?;
        let raw = payload
            .get("embeddings")
            .and_then(|e| e.as_array())
            .ok_or_else(|| EmbedError::Api {
                status: "200".into(),
                body: "в ответе нет массива embeddings".into(),
            })?;
        let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(raw.len());
        for vec in raw {
            let numbers = vec.as_array().map(|a| {
                a.iter()
                    .map(|n| n.as_f64().map(|f| f as f32).unwrap_or(0.0))
                    .collect::<Vec<f32>>()
            });
            let numbers = numbers.ok_or_else(|| EmbedError::Api {
                status: "200".into(),
                body: "embeddings содержит не числовой вектор".into(),
            })?;
            vectors.push(normalize(numbers));
        }
        if let (Some(dim), Some(first)) = (index_dim, vectors.first()) {
            if first.len() != dim {
                return Err(EmbedError::DimMismatch {
                    index_dim: dim,
                    got_dim: first.len(),
                });
            }
        }
        Ok(vectors)
    }
}

/// L2-нормализация: поиск по скалярному произведению нормированных
/// векторов = косинусная близость.
pub fn normalize(mut vector: Vec<f32>) -> Vec<f32> {
    let norm: f32 = vector.iter().map(|c| c * c).sum::<f32>().sqrt();
    if norm > 0.0 {
        for component in &mut vector {
            *component /= norm;
        }
    }
    vector
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}
