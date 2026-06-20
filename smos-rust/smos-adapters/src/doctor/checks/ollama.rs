//! Ollama connectivity + required-models check + optional reranker probe.
//!
//! Two public entry points:
//! - [`check_ollama`] — `GET {url}/api/tags`, list models, match against
//!   the configured upstream/embedding/extraction expectations.
//! - [`check_reranker`] — probe the llama.cpp reranker URL. Reranker is
//!   optional, so unreachable → WARN with a remediation hint, never FAIL.
//!
//! The match logic lives in [`super::super::models`]; this module owns the
//! HTTP IO and the row construction.

use std::time::Duration;

use reqwest::Client;

use super::super::models::{ExpectedModel, match_expected_models};
use super::super::types::CheckResult;
use crate::config::{OllamaConfig, RerankerConfig};

/// Ollama `/api/tags` response shape — only the fields the doctor reads.
/// Extra fields returned by the server are silently ignored by serde.
#[derive(Debug, serde::Deserialize)]
struct TagsResponse {
    models: Vec<TagsModel>,
}

#[derive(Debug, serde::Deserialize)]
struct TagsModel {
    name: String,
}

/// Build the expected-model list from the SMOS config. One row per role:
/// the upstream chat model (granite4.1:3b), the embedding model (Jina v5),
/// and the extraction model (qwen3.5:2b).
pub fn expected_models_from_config(config: &OllamaConfig) -> Vec<ExpectedModel> {
    vec![
        ExpectedModel::new("upstream chat model", "granite4.1:3b"),
        ExpectedModel::new("embedding model", &config.embedding_model),
        ExpectedModel::new("extraction model", &config.extraction_model),
    ]
}

/// Probe Ollama and emit one row per expected model + one connectivity row.
///
/// `timeout` bounds each HTTP request so a wedged Ollama that accepts the
/// TCP handshake but never responds surfaces as FAIL instead of hanging
/// the doctor. Mirrors the per-request cap on [`check_reranker`].
pub async fn check_ollama(
    client: &Client,
    config: &OllamaConfig,
    timeout: Duration,
) -> Vec<CheckResult> {
    let url = format!("{}/api/tags", config.url.trim_end_matches('/'));
    let mut results = Vec::new();

    let response = client.get(&url).timeout(timeout).send().await;
    let body = match response {
        Ok(r) if r.status().is_success() => r.bytes().await.ok(),
        _ => None,
    };

    let Some(bytes) = body else {
        results.push(
            CheckResult::fail("Ollama connectivity", format!("url: {}", config.url))
                .with_recommendation("start `ollama serve`"),
        );
        // Push FAIL rows for every expected model so the operator sees the
        // full delta at once instead of fixing Ollama and re-running.
        for m in expected_models_from_config(config) {
            results.push(
                CheckResult::fail(
                    format!("Required model: {}", m.configured),
                    "Ollama unreachable",
                )
                .with_recommendation(format!("ollama pull {}", m.configured)),
            );
        }
        return results;
    };

    let parsed: Result<TagsResponse, _> = serde_json::from_slice(&bytes);
    let Ok(parsed) = parsed else {
        results.push(
            CheckResult::fail("Ollama connectivity", "response was not valid JSON")
                .with_recommendation("check Ollama version (>=0.1.x)"),
        );
        return results;
    };

    let names: Vec<String> = parsed.models.into_iter().map(|m| m.name).collect();
    let count = names.len();
    results.push(CheckResult::pass(
        "Ollama connectivity",
        format!("url: {}\navailable models: {count}", config.url),
    ));

    let expected = expected_models_from_config(config);
    for (m, hit) in match_expected_models(&expected, &names) {
        let name = format!("Required model: {}", m.configured);
        if hit {
            results.push(CheckResult::pass(name, format!("role: {}", m.role)));
        } else {
            results.push(
                CheckResult::fail(name, "not pulled")
                    .with_recommendation(format!("ollama pull {}", m.configured)),
            );
        }
    }
    results
}

/// Probe the reranker. WARN on any failure — the reranker is optional and
/// the proxy falls back to embedding-only ranking when it is unavailable.
///
/// `timeout` bounds the health probe so an unreachable reranker surfaces
/// as WARN instead of stalling the doctor.
pub async fn check_reranker(
    client: &Client,
    config: &RerankerConfig,
    timeout: Duration,
) -> CheckResult {
    let url = format!("{}/health", config.url.trim_end_matches('/'));
    match client.get(&url).timeout(timeout).send().await {
        Ok(r) if r.status().is_success() => CheckResult::pass(
            "Reranker",
            format!("url: {}\nmodel: {}", config.url, config.model),
        ),
        Ok(r) => CheckResult::warn(
            "Reranker",
            format!("url: {}\nHTTP {}", config.url, r.status()),
        )
        .with_recommendation(
            "reranker optional; start llama.cpp server for improved retrieval quality",
        ),
        Err(_) => CheckResult::warn("Reranker", format!("url: {}\nunreachable", config.url))
            .with_recommendation(
                "reranker optional; start llama.cpp server for improved retrieval quality",
            ),
    }
}

/// Test helper: classify the first model row from an `/api/tags` JSON body.
/// Exposed so the unit tests can verify the parser shape without spinning
/// up Ollama.
#[cfg(test)]
pub(crate) fn parse_tags_for_test(body: &[u8]) -> Option<Vec<String>> {
    let parsed: TagsResponse = serde_json::from_slice(body).ok()?;
    Some(parsed.models.into_iter().map(|m| m.name).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> OllamaConfig {
        OllamaConfig {
            url: "http://localhost:11434".into(),
            embedding_model: "hf.co/jinaai/jina-embeddings-v5:latest".into(),
            extraction_model: "qwen3.5:2b".into(),
            timeout_seconds: 30,
            ..OllamaConfig::default()
        }
    }

    #[test]
    fn expected_models_from_config_lists_all_three_roles() {
        let expected = expected_models_from_config(&cfg());
        assert_eq!(expected.len(), 3);
        assert_eq!(expected[0].role, "upstream chat model");
        assert_eq!(expected[1].role, "embedding model");
        assert_eq!(expected[2].role, "extraction model");
    }

    #[test]
    fn parse_tags_for_test_handles_minimal_body() {
        let body = br#"{"models":[{"name":"granite4.1:3b"},{"name":"qwen3.5:2b"}]}"#;
        let names = parse_tags_for_test(body).expect("parsed");
        assert_eq!(
            names,
            vec!["granite4.1:3b".to_string(), "qwen3.5:2b".to_string()]
        );
    }

    #[test]
    fn parse_tags_for_test_returns_none_on_invalid_body() {
        assert!(parse_tags_for_test(b"not json").is_none());
        assert!(parse_tags_for_test(br#"{"no_models_key":[]}"#).is_none());
    }
}
