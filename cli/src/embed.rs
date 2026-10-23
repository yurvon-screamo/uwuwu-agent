use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const OLLAMA_URL: &str = "http://localhost:11434";
const HTTP_TIMEOUT_SECS: u64 = 300;

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .expect("reqwest client build")
}

#[derive(Serialize)]
struct EmbedRequest<'a, T> {
    model: &'a str,
    input: T,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

pub async fn embed(model: &str, text: &str) -> Result<Vec<f32>> {
    let req = EmbedRequest { model, input: text };

    let resp: EmbedResponse = client()
        .post(format!("{}/api/embed", OLLAMA_URL))
        .json(&req)
        .send()
        .await
        .context("ollama embed request failed")?
        .json()
        .await
        .context("ollama embed response parse failed")?;

    resp.embeddings
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("ollama returned no embeddings"))
}

pub async fn batch_embed(model: &str, texts: &[String]) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(vec![]);
    }

    if texts.len() == 1 {
        return Ok(vec![embed(model, &texts[0]).await?]);
    }

    let req = EmbedRequest {
        model,
        input: texts,
    };

    let resp: EmbedResponse = client()
        .post(format!("{}/api/embed", OLLAMA_URL))
        .json(&req)
        .send()
        .await
        .context("ollama batch embed request failed")?
        .json()
        .await
        .context("ollama batch embed response parse failed")?;

    Ok(resp.embeddings)
}
