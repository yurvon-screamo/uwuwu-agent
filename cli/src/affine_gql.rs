//! Direct AFFiNE GraphQL access for comment replies. The MCP gateway has no
//! reply tool (upstream affine-mcp-server v3.11.0 exposes only list/create/
//! update/delete/resolve), so thread replies go straight to the AFFiNE UI API
//! with the bot's UI credentials — the same sign-in pattern as ServerSearch.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};

use crate::affine_comments::text_to_snapshot;
use crate::affine_search_server::ServerSearchConfig;

pub struct AffineReplies {
    http: reqwest::Client,
    ui_url: String,
}

impl AffineReplies {
    /// One reply session: sign in with email/password, keep the cookie jar.
    pub async fn connect(config: &ServerSearchConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .cookie_store(true)
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .context("http client for affine ui")?;
        let resp = http
            .post(format!("{}/api/auth/sign-in", config.ui_url))
            .json(&json!({"email": config.email, "password": config.password}))
            .send()
            .await
            .context("affine sign-in request")?;
        if !resp.status().is_success() {
            anyhow::bail!("affine sign-in failed: HTTP {}", resp.status());
        }
        Ok(Self {
            http,
            ui_url: config.ui_url.clone(),
        })
    }

    /// Reply into an existing thread: comment_id is the top-level comment id
    /// (see CommentThread). Returns the created reply id.
    pub async fn create_reply(
        &self,
        comment_id: &str,
        text: &str,
        doc_title: &str,
    ) -> Result<String> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let body = json!({
            "query": "mutation CreateReply($input: ReplyCreateInput!) { createReply(input: $input) { id commentId } }",
            "variables": {
                "input": {
                    "commentId": comment_id,
                    "content": text_to_snapshot(text, &snapshot_meta_id(comment_id, now_ms), now_ms),
                    "docMode": "page",
                    "docTitle": doc_title,
                }
            }
        });
        let resp = self
            .http
            .post(format!("{}/graphql", self.ui_url))
            .json(&body)
            .send()
            .await
            .context("affine createReply request")?;
        let status = resp.status();
        let payload: Value = resp.json().await.context("createReply response json")?;
        if !status.is_success() {
            anyhow::bail!("affine createReply HTTP {status}");
        }
        if let Some(errs) = payload.get("errors").and_then(|v| v.as_array()) {
            let msg = errs
                .first()
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("unknown graphql error");
            return Err(anyhow::anyhow!("affine createReply: {msg}"));
        }
        payload
            .pointer("/data/createReply/id")
            .and_then(|id| id.as_str())
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("createReply: ответ без id: {payload:.200}"))
    }
}

/// 10-char alphanumeric id for the snapshot meta (the gateway generates a
/// similar secureAffineId); derived from comment id + clock, enough for a
/// per-comment meta label.
fn snapshot_meta_id(comment_id: &str, now_ms: i64) -> String {
    let mut hasher = Sha1::new();
    hasher.update(comment_id.as_bytes());
    hasher.update(now_ms.to_le_bytes());
    let digest = hasher.finalize();
    let alphabet: Vec<u8> = (b'a'..=b'z').chain(b'0'..=b'9').collect();
    digest
        .iter()
        .filter_map(|b| alphabet.get((*b % 36) as usize))
        .take(10)
        .map(|b| *b as char)
        .collect()
}
