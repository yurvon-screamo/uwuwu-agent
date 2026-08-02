use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub(crate) const OLLAMA_URL: &str = "http://localhost:11434";
pub(crate) const MODEL_ID: &str = "hf.co/SupraLabs/reasoning-summarizer-800m-pre-gguf:Q8_0";
const NUM_PREDICT: u32 = 160;

#[derive(Serialize)]
struct GenerateOptions {
    num_predict: u32,
    temperature: f32,
    top_k: u32,
    seed: u64,
}

#[derive(Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: String,
    raw: bool,
    stream: bool,
    options: GenerateOptions,
}

#[derive(Deserialize)]
struct GenerateResponse {
    response: String,
}

#[derive(Deserialize, Default)]
pub(crate) struct Summary {
    #[serde(default)]
    pub(crate) title: String,
    #[serde(default, rename = "sub_title")]
    pub(crate) sub_title: String,
}

pub(crate) fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .context("cannot build HTTP client")
}

pub(crate) async fn ensure_model_available(client: &reqwest::Client) -> Result<()> {
    let resp = client
        .post(format!("{OLLAMA_URL}/api/show"))
        .json(&serde_json::json!({ "model": MODEL_ID }))
        .send()
        .await
        .context("cannot reach Ollama: is `ollama serve` running?")?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("model not found: {MODEL_ID}\nrun: ollama pull {MODEL_ID}");
    }
    Ok(())
}

pub(crate) async fn summarize(client: &reqwest::Client, text: &str) -> Result<Summary> {
    let req = GenerateRequest {
        model: MODEL_ID,
        prompt: format!("{text}\n"),
        raw: true,
        stream: false,
        options: GenerateOptions {
            num_predict: NUM_PREDICT,
            temperature: 0.0,
            top_k: 1,
            seed: 42,
        },
    };

    let resp: GenerateResponse = client
        .post(format!("{OLLAMA_URL}/api/generate"))
        .json(&req)
        .send()
        .await
        .context("ollama generate request failed")?
        .json()
        .await
        .context("ollama generate response parse failed")?;

    Ok(parse_summary(&resp.response))
}

fn parse_summary(raw: &str) -> Summary {
    if let Ok(s) = serde_json::from_str::<Summary>(raw) {
        return s;
    }
    if let (Some(start), Some(end)) = (raw.find('{'), raw.rfind('}')) {
        if start <= end {
            if let Ok(s) = serde_json::from_str::<Summary>(&raw[start..=end]) {
                return s;
            }
        }
    }
    Summary::default()
}
