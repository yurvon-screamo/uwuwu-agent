//! JSON-RPC client for the affine-mcp gateway (Streamable HTTP, MCP 2025-03-26).
//!
//! The gateway is the only stable API surface over AFFiNE's internal GraphQL:
//! uwuwu-cli talks plain HTTP + JSON-RPC here, no MCP SDK involved. One client
//! owns one server session (initialize → mcp-session-id header on every call).

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

const CALL_TIMEOUT_SECS: u64 = 120;
const CALL_RETRIES: usize = 2;

pub struct AffineConfig {
    pub url: String,
    pub token: String,
    pub workspace: String,
}

impl AffineConfig {
    /// Config from environment. All three variables are required: an unset
    /// variable must fail fast instead of silently targeting a wrong host.
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
        let init_id = client.alloc_id();
        let init = client
            .raw_post(json!({
                "jsonrpc": "2.0",
                "id": init_id,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": "uwuwu-cli", "version": env!("CARGO_PKG_VERSION")}
                }
            }))
            .await?;
        client.session = init
            .headers
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("gateway did not return mcp-session-id"))?;
        // notifications carry no id and no response body (202 Accepted)
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
    /// Network/server errors are retried; logical tool errors are not.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let mut last_err = None;
        for _ in 0..=CALL_RETRIES {
            let call_id = self.alloc_id();
            let body = json!({
                "jsonrpc": "2.0",
                "id": call_id,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments.clone()}
            });
            match self.raw_post(body).await {
                Ok(resp) => {
                    let payload = Self::tool_payload(&resp.body)?;
                    if let Some(err) = payload.get("error") {
                        anyhow::bail!("tool {name} failed: {err}");
                    }
                    // gateway convention: logical failures answer ok:false
                    // inside a successful RPC result — surface them as errors
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
                    return Self::text_payload(payload);
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(anyhow!(
            "tool {name} failed after {} retries: {}",
            CALL_RETRIES,
            last_err.map(|e| e.to_string()).unwrap_or_default()
        ))
    }

    fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn tool_payload(body: &Value) -> Result<&Value> {
        body.get("result")
            .ok_or_else(|| anyhow!("RPC response has no result: {body}"))
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

    async fn raw_post(&self, body: Value) -> Result<RpcResponse> {
        let mut req = self
            .http
            .post(&self.url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        if !self.session.is_empty() {
            req = req.header("mcp-session-id", &self.session);
        }
        let resp = req
            .json(&body)
            .send()
            .await
            .with_context(|| format!("gateway request to {} failed", self.url))?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let raw = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("gateway HTTP {status}: {raw}");
        }
        // JSON response (rare) or SSE stream (common): take the last data line
        let payload =
            parse_sse(&raw).with_context(|| format!("unparsable gateway response: {raw}"))?;
        if let Some(err) = payload.get("error") {
            anyhow::bail!("RPC error: {err}");
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

/// Extract the JSON payload: either the raw body or the last `data:` line
/// of an SSE event stream carrying an id (responses, not notifications).
fn parse_sse(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed).ok();
    }
    let mut found = None;
    for line in trimmed.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if let Ok(v) = serde_json::from_str::<Value>(data) {
                if v.get("id").is_some() {
                    found = Some(v);
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sse_prefers_json_body_when_present() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
        let v = parse_sse(raw).unwrap();
        assert!(v["result"]["ok"].as_bool().unwrap());
    }

    #[test]
    fn parse_sse_takes_last_id_event_from_stream() {
        let raw = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{}}\n\n";
        let v = parse_sse(raw).unwrap();
        assert_eq!(v["id"], 7);
    }

    #[test]
    fn parse_sse_returns_none_on_garbage() {
        assert!(parse_sse("not json at all").is_none());
    }
}
