//! `ReqwestUpstream` — OpenAI-compatible HTTP upstream via `reqwest`.
//!
//! Forwards a `ChatRequest` to the configured upstream URL and returns either:
//! - `ChatResponse::Streaming(bytes_stream)` when `request.is_streaming()`, or
//! - `ChatResponse::NonStreaming(json)` otherwise.
//!
//! The body is serialised from `ChatRequest` (its `#[serde(flatten)] extra`
//! keeps every OpenAI parameter intact on the wire). Auth uses a `Bearer`
//! token by default; the header name is configurable to support Azure-style
//! `api-key` headers.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use reqwest::Client;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use smos_application::errors::UpstreamError;
use smos_application::ports::LlmUpstream;
use smos_application::types::{ChatRequest, ChatResponse};

use crate::config::UpstreamConfig;

/// HTTP upstream backed by a pooled `reqwest::Client`.
#[derive(Clone)]
pub struct ReqwestUpstream {
    client: Client,
    config: Arc<UpstreamConfig>,
}

impl ReqwestUpstream {
    /// Build a new upstream with a request timeout configured from
    /// `config.timeout_seconds`. Validates `api_key` (if non-empty) up front
    /// so a misconfigured secret with control characters fails fast at startup
    /// rather than silently producing an unauthenticated request later.
    pub fn new(config: Arc<UpstreamConfig>) -> Result<Self, UpstreamError> {
        if !config.api_key.is_empty()
            && let Err(e) = HeaderValue::from_str(&config.api_key)
        {
            return Err(UpstreamError::ConnectFailed(format!(
                "api_key contains invalid header bytes: {e}"
            )));
        }
        let timeout = Duration::from_secs(config.timeout_seconds);
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| UpstreamError::ConnectFailed(e.to_string()))?;
        Ok(Self { client, config })
    }

    fn build_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if !self.config.api_key.is_empty() {
            // Azure-style `api-key:` header carries the raw key; every other
            // header name uses the `Authorization: Bearer <key>` scheme.
            let is_api_key_header = self.config.auth_header.eq_ignore_ascii_case("api-key");
            let value = if is_api_key_header {
                HeaderValue::from_str(&self.config.api_key)
            } else {
                HeaderValue::from_str(&format!("Bearer {}", self.config.api_key))
            }
            .expect("api_key validated at construction");
            let name = safe_header_name(&self.config.auth_header);
            headers.insert(name, value);
        }
        headers
    }

    async fn send(&self, request: &ChatRequest) -> Result<reqwest::Response, UpstreamError> {
        let body = serde_json::to_value(request)
            .map_err(|e| UpstreamError::SerializationError(e.to_string()))?;
        let response = self
            .client
            .post(&self.config.url)
            .headers(self.build_headers())
            .json(&body)
            .send()
            .await
            .map_err(|e| map_send_error(e, self.config.timeout_seconds))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(UpstreamError::StatusError {
                status: status.as_u16(),
                body: text,
            });
        }
        Ok(response)
    }
}

impl LlmUpstream for ReqwestUpstream {
    async fn complete(&self, request: ChatRequest) -> Result<ChatResponse, UpstreamError> {
        let is_streaming = request.is_streaming();
        let response = self.send(&request).await?;
        if is_streaming {
            let stream = response
                .bytes_stream()
                .map(|item| item.map_err(|e| UpstreamError::StreamError(e.to_string())));
            Ok(ChatResponse::Streaming(Box::new(stream)))
        } else {
            let value = response
                .json::<serde_json::Value>()
                .await
                .map_err(|e| UpstreamError::BadResponse(e.to_string()))?;
            Ok(ChatResponse::NonStreaming(value))
        }
    }
}

/// Classify a connection-level `reqwest` error into the upstream error that
/// best matches its cause. Timeouts surface as `Timeout` (carrying the
/// configured duration so logs/dashboards read correctly); everything else
/// (DNS, connect, TLS) is `ConnectFailed`.
fn map_send_error(e: reqwest::Error, timeout_seconds: u64) -> UpstreamError {
    if e.is_timeout() {
        UpstreamError::Timeout(Duration::from_secs(timeout_seconds))
    } else {
        UpstreamError::ConnectFailed(e.to_string())
    }
}

/// Build a `HeaderName` from a user-supplied header string, defaulting to
/// `Authorization`. Unknown characters fall back to the canonical header so a
/// malformed config never panics at request time; the fallback is logged once
/// per call so operators notice a misconfigured `auth_header`.
fn safe_header_name(name: &str) -> HeaderName {
    match HeaderName::try_from(name) {
        Ok(n) => n,
        Err(_) => {
            tracing::warn!(
                auth_header = name,
                "invalid auth_header config; falling back to Authorization"
            );
            AUTHORIZATION
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Arc<UpstreamConfig> {
        Arc::new(UpstreamConfig {
            url: "http://127.0.0.1:1/v1/chat/completions".into(),
            api_key: "ollama".into(),
            auth_header: "Authorization".into(),
            timeout_seconds: 1,
        })
    }

    #[test]
    fn build_headers_adds_bearer_authorization() {
        let upstream = ReqwestUpstream::new(cfg()).expect("build");
        let headers = upstream.build_headers();
        let auth = headers.get(AUTHORIZATION).expect("auth header present");
        assert_eq!(auth, "Bearer ollama");
    }

    #[test]
    fn build_headers_omits_authorization_when_api_key_empty() {
        let mut c = (*cfg()).clone();
        c.api_key = String::new();
        let upstream = ReqwestUpstream::new(Arc::new(c)).expect("build");
        let headers = upstream.build_headers();
        assert!(headers.get(AUTHORIZATION).is_none());
    }

    #[test]
    fn build_headers_uses_api_key_header_name_when_configured() {
        let mut c = (*cfg()).clone();
        c.auth_header = "api-key".into();
        let upstream = ReqwestUpstream::new(Arc::new(c)).expect("build");
        let headers = upstream.build_headers();
        // Azure-style `api-key:` header carries the raw key (no Bearer prefix).
        let header = headers.get("api-key").expect("api-key header present");
        assert_eq!(header, "ollama");
    }

    #[test]
    fn new_rejects_api_key_with_invalid_header_bytes() {
        // Control characters are illegal in HTTP header values; the fail-fast
        // validation in `new` must surface this at construction time.
        let mut c = (*cfg()).clone();
        c.api_key = "bad\u{0000}key".into();
        match ReqwestUpstream::new(Arc::new(c)) {
            Err(err) => assert!(
                err.to_string().contains("api_key"),
                "expected api_key in error: {err}"
            ),
            Ok(_) => panic!("expected construction to fail for an invalid api_key"),
        }
    }

    #[test]
    fn safe_header_name_falls_back_to_authorization_for_invalid_input() {
        assert_eq!(safe_header_name("not a valid header"), AUTHORIZATION);
        assert_eq!(safe_header_name("api-key"), "api-key");
    }
}
