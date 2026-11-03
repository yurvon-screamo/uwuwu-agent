//! Tests for affine_comments: nested thread parsing, snapshot building,
//! thread-id prefix resolution.

use crate::affine_comments::{parse_comment_connection, resolve_thread_prefix, text_to_snapshot};
use serde_json::{json, Value};

fn comment_node(id: &str, user_id: &str, text: &str, replies: Value) -> Value {
    json!({
        "node": {
            "id": id,
            "resolved": false,
            "content": {
                "preview": text,
                "snapshot": {"meta": {"createDate": 1793000000000i64}}
            },
            "user": {"id": user_id},
            "replies": replies,
        }
    })
}

fn reply_node(id: &str, user_id: &str, text: &str) -> Value {
    json!({
        "id": id,
        "content": {
            "preview": "",
            "snapshot": {
                "meta": {"createDate": 1793000001000i64},
                "blocks": {
                    "children": [{
                        "children": [{
                            "props": {"text": {"delta": [{"insert": text}]}}
                        }]
                    }]
                }
            }
        },
        "user": {"id": user_id}
    })
}

#[test]
fn comment_connection_with_replies_parses_into_threads() {
    let resp = json!({
        "edges": [
            comment_node("c-1", "user-a", "вопрос юзера",
                json!([reply_node("r-1", "bot", "ответ агента")])),
            comment_node("c-2", "bot", "статусный коммент", json!([])),
        ]
    });

    let threads = parse_comment_connection(&resp);

    assert_eq!(threads.len(), 2);
    assert_eq!(threads[0].id, "c-1");
    assert_eq!(threads[0].text, "вопрос юзера");
    assert_eq!(threads[0].user_id, "user-a");
    assert_eq!(threads[0].replies.len(), 1);
    assert_eq!(threads[0].replies[0].id, "r-1");
    assert_eq!(threads[0].replies[0].text, "ответ агента");
    assert_eq!(threads[0].replies[0].user_id, "bot");
    assert!(threads[1].replies.is_empty());
}

#[test]
fn reply_text_falls_back_to_snapshot_when_preview_is_empty() {
    // reply_node кладёт текст только в snapshot-дельты
    let resp = json!({
        "edges": [comment_node("c-1", "user-a", "тема",
            json!([reply_node("r-1", "bot", "найденный в дельтах текст")]))]
    });

    let threads = parse_comment_connection(&resp);

    assert_eq!(threads[0].replies[0].text, "найденный в дельтах текст");
}

#[test]
fn resolved_flag_is_carried_from_comment_node() {
    let mut node = comment_node("c-1", "user-a", "тема", json!([]));
    node["node"]["resolved"] = json!(true);
    let resp = json!({"edges": [node]});

    let threads = parse_comment_connection(&resp);

    assert!(threads[0].resolved);
}

#[test]
fn empty_connection_yields_no_threads() {
    let threads = parse_comment_connection(&json!({"edges": []}));

    assert!(threads.is_empty());
}

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

fn sample_threads() -> Vec<crate::affine_comments::CommentThread> {
    parse_comment_connection(&json!({
        "edges": [
            comment_node("aaaa2222-3333-4444-5555-666677778888", "user-a", "первая",
                json!([])),
            comment_node("bbbb1111-3333-4444-5555-666677778888", "bot", "вторая",
                json!([])),
        ]
    }))
}

#[test]
fn thread_prefix_resolves_uniquely_ignoring_dashes() {
    let threads = sample_threads();

    let found = resolve_thread_prefix(&threads, "aaaa22223333").unwrap();

    assert_eq!(found.id, "aaaa2222-3333-4444-5555-666677778888");
}

#[test]
fn ambiguous_thread_prefix_errors_with_candidates() {
    let threads = sample_threads();

    let err = resolve_thread_prefix(&threads, "").unwrap_err().to_string();

    assert!(err.contains("первая") && err.contains("вторая"), "{err}");
}

#[test]
fn unknown_thread_prefix_errors() {
    let threads = sample_threads();

    let err = resolve_thread_prefix(&threads, "ffff0000")
        .unwrap_err()
        .to_string();

    assert!(err.contains("не найден"), "{err}");
}
