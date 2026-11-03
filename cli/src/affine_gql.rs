//! Direct AFFiNE GraphQL access for comment replies. The MCP gateway has no
//! reply tool (upstream affine-mcp-server v3.11.0 exposes only list/create/
//! update/delete/resolve), so thread replies go straight to the AFFiNE UI API
//! with the bot's UI credentials — the same sign-in pattern as ServerSearch.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};

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
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("affine createReply HTTP {status}: {}", cut(&text, 200));
        }
        let payload: Value = resp.json().await.context("createReply response json")?;
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
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "createReply: ответ без id: {}",
                    cut(&payload.to_string(), 200)
                )
            })
    }
}

/// Plain text → BlockSuite snapshot (page → note → paragraph), the shape the
/// gateway's normalizeCommentContent produces and the AFFiNE comment editor
/// renders. Newlines stay inside a single delta insert, like the gateway.
fn text_to_snapshot(text: &str, meta_id: &str, create_date_ms: i64) -> Value {
    let delta = if text.is_empty() {
        vec![]
    } else {
        vec![json!({"insert": text})]
    };
    json!({
        "snapshot": {
            "type": "page",
            "meta": {"id": meta_id, "title": "", "createDate": create_date_ms, "tags": []},
            "blocks": {
                "type": "block", "id": "page", "flavour": "affine:page", "version": 2,
                "props": {"title": {"$blocksuite:internal:text$": true, "delta": []}},
                "children": [{
                    "type": "block", "id": "note", "flavour": "affine:note", "version": 1,
                    "props": {"displayMode": "both"},
                    "children": [{
                        "type": "block", "id": "paragraph", "flavour": "affine:paragraph",
                        "version": 1,
                        "props": {
                            "type": "text", "collapsed": false,
                            "text": {"$blocksuite:internal:text$": true, "delta": delta},
                        },
                        "children": [],
                    }],
                }],
            },
        }
    })
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
        .map(|b| alphabet[(*b % 36) as usize] as char)
        .take(10)
        .collect()
}

fn cut(s: &str, max_chars: usize) -> String {
    s.char_indices()
        .nth(max_chars)
        .map_or_else(|| s.to_string(), |(i, _)| format!("{}…", &s[..i]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_becomes_blocksuite_page_snapshot() {
        let snapshot = text_to_snapshot("ответ агента", "abc123xyz0", 1793000000000i64);

        let s = &snapshot["snapshot"];
        assert_eq!(s["type"], "page");
        assert_eq!(s["meta"]["id"], "abc123xyz0");
        assert_eq!(s["meta"]["createDate"], 1793000000000i64);
        assert_eq!(s["blocks"]["flavour"], "affine:page");
        let note = &s["blocks"]["children"][0];
        assert_eq!(note["flavour"], "affine:note");
        let paragraph = &note["children"][0];
        assert_eq!(paragraph["flavour"], "affine:paragraph");
        assert_eq!(
            paragraph["props"]["text"]["delta"][0]["insert"],
            "ответ агента"
        );
    }

    #[test]
    fn empty_text_snapshot_has_empty_delta() {
        let snapshot = text_to_snapshot("", "abc123xyz0", 1793000000000i64);

        let delta =
            &snapshot["snapshot"]["blocks"]["children"][0]["children"][0]["props"]["text"]["delta"];
        assert_eq!(delta.as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn cut_truncates_long_strings_with_ellipsis() {
        let long = "a".repeat(300);

        assert_eq!(cut(&long, 200).chars().count(), 201);
        assert!(cut("коротко", 200).ends_with("коротко"));
    }
}
