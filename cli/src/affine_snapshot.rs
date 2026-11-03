//! BlockSuite page snapshot for comment/reply content: the shape the
//! gateway's normalizeCommentContent produces and the AFFiNE comment editor
//! renders. Newlines stay inside a single delta insert, like the gateway.

use serde_json::{json, Value};
use sha1::{Digest, Sha1};

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

/// 10-char alphanumeric id for the snapshot meta (the gateway generates a
/// similar secureAffineId); derived from comment id + clock, enough for a
/// per-comment meta label.
pub fn snapshot_meta_id(comment_id: &str, now_ms: i64) -> String {
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
    fn snapshot_meta_id_is_ten_alphanumeric_chars() {
        let id = snapshot_meta_id("e39eb023", 1793000000000i64);

        assert_eq!(id.len(), 10);
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_eq!(
            snapshot_meta_id("e39eb023", 1793000000000i64),
            snapshot_meta_id("e39eb023", 1793000000000i64)
        );
    }
}
