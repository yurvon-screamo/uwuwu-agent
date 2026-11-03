//! Comment threads: AFFiNE stores top-level comments with nested replies
//! (postgres `replies`, GraphQL `createReply`). The gateway's list_comments
//! returns replies nested in each comment node; this module parses that
//! shape into threads, builds BlockSuite snapshots for new content, and
//! resolves thread-id prefixes.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// A reply nested in a comment thread.
#[derive(Debug)]
pub struct CommentReply {
    pub id: String,
    pub text: String,
    pub user_id: String,
    pub created: i64,
}

/// A top-level comment — the root of a thread — with its nested replies.
#[derive(Debug)]
pub struct CommentThread {
    pub id: String,
    pub text: String,
    pub user_id: String,
    pub created: i64,
    pub resolved: bool,
    pub replies: Vec<CommentReply>,
}

/// Parse a list_comments connection: edges → threads with nested replies.
pub fn parse_comment_connection(resp: &Value) -> Vec<CommentThread> {
    let edges = match resp.get("edges").and_then(|e| e.as_array()) {
        Some(edges) => edges,
        None => return Vec::new(),
    };
    edges
        .iter()
        .filter_map(|e| e.get("node"))
        .map(parse_thread)
        .collect()
}

fn parse_thread(node: &Value) -> CommentThread {
    let content = node.get("content").cloned().unwrap_or(Value::Null);
    let replies = node
        .get("replies")
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().map(parse_reply).collect())
        .unwrap_or_default();
    CommentThread {
        id: first_str(node, &["id"]),
        text: comment_text(&content),
        user_id: node
            .pointer("/user/id")
            .and_then(|u| u.as_str())
            .unwrap_or_default()
            .into(),
        created: content
            .pointer("/snapshot/meta/createDate")
            .and_then(|c| c.as_i64())
            .unwrap_or(0),
        resolved: node
            .get("resolved")
            .and_then(|r| r.as_bool())
            .unwrap_or(false),
        replies,
    }
}

fn parse_reply(node: &Value) -> CommentReply {
    CommentReply {
        id: first_str(node, &["id"]),
        text: comment_text(&node.get("content").cloned().unwrap_or(Value::Null)),
        user_id: node
            .pointer("/user/id")
            .and_then(|u| u.as_str())
            .unwrap_or_default()
            .into(),
        created: node
            .pointer("/content/snapshot/meta/createDate")
            .and_then(|c| c.as_i64())
            .unwrap_or(0),
    }
}

/// Comment text: non-empty preview wins, otherwise walk the snapshot deltas.
fn comment_text(content: &Value) -> String {
    let preview = content
        .get("preview")
        .and_then(|p| p.as_str())
        .unwrap_or_default();
    if !preview.trim().is_empty() {
        return preview.to_string();
    }
    snapshot_text(content)
}

/// Concatenate all text deltas from a comment's snapshot block tree.
fn snapshot_text(content: &Value) -> String {
    fn walk(node: &Value, out: &mut String) {
        if let Some(delta) = node.pointer("/props/text/delta").and_then(|d| d.as_array()) {
            for d in delta {
                if let Some(ins) = d.get("insert").and_then(|i| i.as_str()) {
                    out.push_str(ins);
                }
            }
        }
        if let Some(children) = node.get("children").and_then(|c| c.as_array()) {
            for ch in children {
                walk(ch, out);
            }
        }
    }
    let mut out = String::new();
    if let Some(blocks) = content.pointer("/snapshot/blocks") {
        walk(blocks, &mut out);
    }
    out.trim().to_string()
}

/// Plain text → BlockSuite snapshot (page → note → paragraph), the shape the
/// gateway's normalizeCommentContent produces and the AFFiNE comment editor
/// renders. Newlines stay inside a single delta insert, like the gateway.
pub fn text_to_snapshot(text: &str, meta_id: &str, create_date_ms: i64) -> Value {
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

/// Resolve a thread by id prefix; dashes are ignored (same convention as
/// row-id addressing). Ambiguity and misses carry the visible candidates.
pub fn resolve_thread_prefix<'a>(
    threads: &'a [CommentThread],
    prefix: &str,
) -> Result<&'a CommentThread> {
    let needle = prefix.replace('-', "").to_lowercase();
    let matches: Vec<&CommentThread> = threads
        .iter()
        .filter(|t| t.id.replace('-', "").to_lowercase().starts_with(&needle))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(anyhow!(
            "тред с префиксом «{prefix}» не найден (task thread покажет id)"
        )),
        many => {
            let heads: Vec<String> = many
                .iter()
                .map(|t| format!("#{} {}", short_id(&t.id), quoted_head(&t.text)))
                .collect();
            Err(anyhow!(
                "префикс «{prefix}» неоднозначен, кандидаты:\n{}",
                heads.join("\n")
            ))
        }
    }
}

/// Short thread id for display: first 8 chars of the id without dashes.
pub fn short_id(id: &str) -> String {
    id.chars().filter(|c| *c != '-').take(8).collect()
}

/// Первая строка текста в «кавычках», до 60 символов — для списков кандидатов.
fn quoted_head(text: &str) -> String {
    let cut = first_line(text)
        .char_indices()
        .nth(60)
        .map(|(i, _)| i)
        .unwrap_or_else(|| first_line(text).len());
    format!("«{}»", &first_line(text)[..cut])
}

/// Первая строка текста — для однострочных превью.
pub(crate) fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

fn first_str(v: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(|x| x.as_str()))
        .unwrap_or_default()
        .to_string()
}
