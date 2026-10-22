//! JSON-RPC client for the affine-mcp gateway (Streamable HTTP, MCP 2025-03-26).
//!
//! The gateway is the only stable API surface over AFFiNE's internal GraphQL:
//! uwuwu-cli talks plain HTTP + JSON-RPC here, no MCP SDK involved. One client
//! owns one server session (initialize → mcp-session-id header on every call).
//!
//! Slightly over the 200-line module limit: session lifecycle, retry policy
//! and error classification are one cohesive transport contract; extracting
//! any part would force the caller to understand transport internals.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::affine_sse;

const CALL_TIMEOUT_SECS: u64 = 120;
/// Retries cover transport failures only: connection errors and 5xx/429.
/// Timeouts are NOT retried — the server may have applied the write, and
/// replaying non-idempotent tools (create/append) would duplicate content.
const CALL_RETRIES: usize = 2;

pub struct AffineConfig {
    pub url: String,
    pub token: String,
    pub workspace: String,
}

impl AffineConfig {
    /// URL defaults to the local gateway; token and workspace must be set.
    pub fn from_env() -> Result<Self> {
        let url = std::env::var("UWUWU_AFFINE_URL")
            .unwrap_or_else(|_| "http://localhost:3000/mcp".to_string());
        let token = std::env::var("UWUWU_AFFINE_TOKEN")
            .map_err(|_| anyhow!("UWUWU_AFFINE_TOKEN is not set"))?;
        let workspace = std::env::var("UWUWU_AFFINE_WORKSPACE_ID")
            .map_err(|_| anyhow!("UWUWU_AFFINE_WORKSPACE_ID is not set"))?;
        Ok(Self {
            url,
            token,
            workspace,
        })
    }
}

pub struct AffineClient {
    url: String,
    token: String,
    session: String,
    http: reqwest::Client,
    next_id: u64,
}

impl AffineClient {
    /// Open a session: initialize + initialized notification.
    pub async fn connect(cfg: &AffineConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(CALL_TIMEOUT_SECS))
            .build()?;
        let mut client = Self {
            url: cfg.url.clone(),
            token: cfg.token.clone(),
            session: String::new(),
            http,
            next_id: 0,
        };
        let init = client
            .request_with_retry(
                "initialize",
                json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": "uwuwu-cli", "version": env!("CARGO_PKG_VERSION")}
                }),
            )
            .await?;
        client.session = init
            .headers
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("gateway did not return mcp-session-id"))?;
        client
            .post_notification(json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }))
            .await?;
        Ok(client)
    }

    /// Call a gateway tool; returns its structured output (or the parsed
    /// text payload when the server sent no structuredContent).
    /// Transport failures (connect, 5xx, 429) are retried; logical tool
    /// errors and timeouts fail fast.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let resp = self
            .request_with_retry("tools/call", json!({"name": name, "arguments": arguments}))
            .await?;
        Self::tool_result(name, &resp.body)
    }

    /// POST a request until it answers; only transport errors repeat.
    async fn request_with_retry(&mut self, method: &str, params: Value) -> Result<RpcResponse> {
        let mut last_err = None;
        for _ in 0..=CALL_RETRIES {
            let call_id = self.alloc_id();
            let body = json!({
                "jsonrpc": "2.0",
                "id": call_id,
                "method": method,
                "params": params
            });
            match self.raw_post(body, call_id).await {
                Ok(resp) => return Ok(resp),
                Err(GatewayError::Transport(e)) => last_err = Some(e),
                Err(GatewayError::Fatal(e)) => return Err(e),
            }
        }
        Err(anyhow!(
            "{method} failed after {CALL_RETRIES} retries: {}",
            last_err.map(|e| e.to_string()).unwrap_or_default()
        ))
    }

    fn tool_result(name: &str, body: &Value) -> Result<Value> {
        let payload = body
            .get("result")
            .ok_or_else(|| anyhow!("RPC response has no result: {body}"))?;
        if let Some(err) = payload.get("error") {
            anyhow::bail!("tool {name} failed: {err}");
        }
        // gateway convention: logical failures answer ok:false inside a
        // successful RPC result — surface them as errors
        if payload.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            let detail = payload
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown tool error");
            anyhow::bail!("tool {name} failed: {detail}");
        }
        if let Some(sc) = payload.get("structuredContent") {
            return Ok(sc.clone());
        }
        Self::text_payload(payload)
    }

    /// Tools answer either with structuredContent or with a text blob that
    /// itself contains JSON (the gateway's style). Non-JSON text is wrapped.
    fn text_payload(payload: &Value) -> Result<Value> {
        let text = payload
            .get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow!("tool result has neither structuredContent nor text"))?;
        Ok(serde_json::from_str::<Value>(text).unwrap_or_else(|_| Value::String(text.to_string())))
    }

    fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Fire-and-forget notification: 202 with an empty body is success.
    async fn post_notification(&self, body: Value) -> Result<()> {
        let resp = self
            .http
            .post(&self.url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("mcp-session-id", &self.session)
            .json(&body)
            .send()
            .await
            .with_context(|| "gateway notification failed")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("gateway HTTP {status}: {text}");
        }
        Ok(())
    }

    async fn raw_post(
        &self,
        body: Value,
        expected_id: u64,
    ) -> std::result::Result<RpcResponse, GatewayError> {
        let mut req = self
            .http
            .post(&self.url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        if !self.session.is_empty() {
            req = req.header("mcp-session-id", &self.session);
        }
        let resp = match req.json(&body).send().await {
            Ok(resp) => resp,
            // timeouts are ambiguous (write may have applied) — never retry
            Err(e) if e.is_timeout() => return Err(GatewayError::Fatal(e.into())),
            Err(e) => return Err(GatewayError::Transport(e.into())),
        };
        let status = resp.status();
        let headers = resp.headers().clone();
        let raw = resp.text().await.unwrap_or_default();
        if status.as_u16() == 429 || status.is_server_error() {
            return Err(GatewayError::Transport(anyhow!("HTTP {status}: {raw}")));
        }
        if !status.is_success() {
            return Err(GatewayError::Fatal(anyhow!("HTTP {status}: {raw}")));
        }
        let payload = affine_sse::parse_response(&raw, expected_id)
            .map_err(|e| GatewayError::Fatal(anyhow!("unparsable gateway response: {e}")))?;
        if let Some(err) = payload.get("error") {
            return Err(GatewayError::Fatal(anyhow!("RPC error: {err}")));
        }
        Ok(RpcResponse {
            body: payload,
            headers,
        })
    }
}

struct RpcResponse {
    body: Value,
    headers: reqwest::header::HeaderMap,
}

enum GatewayError {
    Transport(anyhow::Error),
    Fatal(anyhow::Error),
}
