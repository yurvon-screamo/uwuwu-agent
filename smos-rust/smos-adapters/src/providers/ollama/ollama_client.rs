//! Shared HTTP helpers for Ollama requests.
//!
//! One `reqwest::Client` per adapter is the right granularity: the client pools
//! connections and amortises TLS handshakes across requests. The builder is
//! configured with the Ollama timeout from [`crate::config::OllamaConfig`].

use std::time::Duration;

use reqwest::Client;
use smos_application::errors::ProviderError;

use crate::config::OllamaConfig;

/// Build a pooled `reqwest::Client` configured with the supplied timeout.
///
/// Free function so the embedding adapter (and a future Ollama extraction
/// adapter in Slice-5) can share the same builder shape without forcing a
/// shared owner.
pub fn build_client(config: &OllamaConfig) -> Result<Client, ProviderError> {
    Client::builder()
        .timeout(Duration::from_secs(config.timeout_seconds))
        .build()
        .map_err(|e| ProviderError::Unavailable(format!("reqwest client build failed: {e}")))
}
