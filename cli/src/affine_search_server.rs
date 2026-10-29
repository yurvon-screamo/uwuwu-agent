//! Server-side full-text search via the AFFiNE GraphQL API (Manticore backend).
//!
//! Complements the local semantic search: no local Ollama or embedding cache
//! required, always fresh (the workspace indexer feeds Manticore on every
//! change). Falls back to local search when credentials are absent or the
//! endpoint fails — see `lib.rs` for the wiring.
//!
//! Env: `UWUWU_AFFINE_UI_URL` (e.g. https://affine.uwuwu.net),
//! `UWUWU_AFFINE_EMAIL`, `UWUWU_AFFINE_PASSWORD`.

use anyhow::{anyhow, Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::json;

pub struct ServerSearchConfig {
    pub ui_url: String,
    pub email: String,
    pub password: String,
}

impl ServerSearchConfig {
    pub fn from_env() -> Option<Self> {
        // Railway-хост мёртв: игнорируем с подсказкой (креды при этом валидны)
        let mut ui_url = std::env::var("UWUWU_AFFINE_UI_URL").ok()?;
        if ui_url.contains("railway.app") {
            eprintln!(
                "uwuwu-cli: UWUWU_AFFINE_UI_URL={ui_url} — Railway-хост мёртв, использую https://affine.uwuwu.net"
            );
            ui_url = "https://affine.uwuwu.net".to_string();
        }
        let email = std::env::var("UWUWU_AFFINE_EMAIL").ok()?;
        let password = std::env::var("UWUWU_AFFINE_PASSWORD").ok()?;
        if ui_url.is_empty() || email.is_empty() || password.is_empty() {
            return None;
        }
        Some(Self {
            ui_url: ui_url.trim_end_matches('/').to_string(),
            email,
            password,
        })
    }
}

pub struct ServerHit {
    pub doc_id: String,
    pub title: String,
    pub snippet: String,
}

/// One search session: sign in with email/password, keep the cookie jar.
pub struct ServerSearch {
    http: reqwest::Client,
    ui_url: String,
    workspace: String,
}

impl ServerSearch {
    pub async fn connect(ss: &ServerSearchConfig, workspace: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .cookie_store(true)
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .context("http client for affine ui")?;
        let resp = http
            .post(format!("{}/api/auth/sign-in", ss.ui_url))
            .json(&json!({"email": ss.email, "password": ss.password}))
            .send()
            .await
            .context("affine sign-in request")?;
        if !resp.status().is_success() {
            anyhow::bail!("affine sign-in failed: HTTP {}", resp.status());
        }
        Ok(Self {
            http,
            ui_url: ss.ui_url.clone(),
            workspace: workspace.to_string(),
        })
    }

    /// Full-text search over docs (title + plain text). Returns ranked hits.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<ServerHit>> {
        let body = json!({
            "query": "query($ws: String!, $in: SearchInput!) { workspace(id: $ws) { search(input: $in) { nodes { fields highlights } } } }",
            "variables": {
                "ws": self.workspace,
                "in": {
                    "table": "block",
                    "query": {"type": "match", "match": query, "field": "content"},
                    "options": {
                        "fields": ["doc_id", "content", "flavour"],
                        "pagination": {"limit": limit.max(1)}
                    }
                }
            }
        });
        let resp = self
            .http
            .post(format!("{}/graphql", self.ui_url))
            .headers(HeaderMap::from_iter([(
                reqwest::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )]))
            .json(&body)
            .send()
            .await
            .context("affine graphql request")?;
        let status = resp.status();
        let payload: serde_json::Value = resp.json().await.context("graphql response json")?;
        if !status.is_success() {
            anyhow::bail!("affine graphql HTTP {status}");
        }
        if let Some(errs) = payload.get("errors").and_then(|v| v.as_array()) {
            let msg = errs
                .first()
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("unknown graphql error");
            return Err(anyhow!("affine graphql: {msg}"));
        }
        let nodes = payload
            .pointer("/data/workspace/search/nodes")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("unexpected search response shape"))?;
        // Block-level hits: dedupe by doc_id, keep the first snippet each.
        let mut hits: Vec<ServerHit> = Vec::new();
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for node in nodes {
            let fields = node
                .get("fields")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let doc_id = first_string(&fields, &["doc_id", "docId"]);
            if doc_id.is_empty() || !seen.insert(doc_id.clone()) {
                continue;
            }
            // block search returns fields.content[] instead of highlights
            let snippet = node
                .get("highlights")
                .and_then(|h| h.get("content"))
                .and_then(first_of_array)
                .or_else(|| first_string_opt(&fields, &["content"]))
                .unwrap_or_default();
            hits.push(ServerHit {
                doc_id,
                title: String::new(), // resolved from the cache by the caller
                snippet,
            });
        }
        Ok(hits)
    }
}

/// First string value under any of the keys; arrays contribute [0].
fn first_string(fields: &serde_json::Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|k| fields.get(*k))
        .and_then(first_of_array_opt)
        .unwrap_or_default()
}

fn first_string_opt(fields: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| fields.get(*k))
        .and_then(first_of_array_opt)
}

fn first_of_array(v: &serde_json::Value) -> Option<String> {
    first_of_array_opt(v)
}

fn first_of_array_opt(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(arr) => arr.first().and_then(|x| x.as_str()).map(String::from),
        _ => None,
    }
}
