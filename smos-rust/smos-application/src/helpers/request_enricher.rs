//! Request enricher — prepend the memory block to `messages[0].content` (§3
//! step 8).
//!
//! Operates on a JSON-encoded messages array (the OpenAI envelope) so the
//! enrichment logic has no dependency on the application-layer `ChatRequest`
//! type.
//!
//! Behaviour mirrors the POC `inject_into_messages`:
//! - Empty `messages` → inject a synthetic `{role: "system", content: block}`.
//! - String `messages[0].content` → prepend `block\n\n<old content>`.
//! - Array `messages[0].content` → flatten to text then prepend (the upstream
//!   adapter rebuilds multipart if needed).
//! - Empty block → returns the input unchanged (fail-open).

use serde_json::{Map, Value};

use crate::helpers::openai_content::flatten_text;

/// Prepend `block` to the first message's content. Always returns a JSON value.
pub fn inject(messages: &Value, block: &str) -> Value {
    if block.is_empty() {
        return messages.clone();
    }
    let Some(arr) = messages.as_array() else {
        return messages.clone();
    };
    if arr.is_empty() {
        return Value::Array(vec![new_system_message(block)]);
    }
    let mut enriched: Vec<Value> = arr.clone();
    let mut first = enriched[0].clone();
    let original_text = first.get("content").map(flatten_text).unwrap_or_default();
    let combined = if original_text.is_empty() {
        block.to_string()
    } else {
        format!("{block}\n\n{original_text}")
    };
    set_content(&mut first, combined);
    enriched[0] = first;
    Value::Array(enriched)
}

fn new_system_message(block: &str) -> Value {
    let mut map = Map::new();
    map.insert("role".to_string(), Value::String("system".to_string()));
    map.insert("content".to_string(), Value::String(block.to_string()));
    Value::Object(map)
}

fn set_content(message: &mut Value, value: String) {
    if let Value::Object(map) = message {
        map.insert("content".to_string(), Value::String(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_messages_creates_synthetic_system_message() {
        let out = inject(&json!([]), "block");
        let first = &out.as_array().unwrap()[0];
        assert_eq!(first.get("role").and_then(Value::as_str), Some("system"));
        assert_eq!(first.get("content").and_then(Value::as_str), Some("block"));
    }

    #[test]
    fn prepends_block_to_existing_string_content() {
        let messages = json!([{ "role": "user", "content": "hello" }]);
        let out = inject(&messages, "BLOCK");
        assert_eq!(
            out.as_array().unwrap()[0]
                .get("content")
                .and_then(Value::as_str),
            Some("BLOCK\n\nhello")
        );
    }

    #[test]
    fn prepends_block_when_existing_content_is_empty() {
        let messages = json!([{ "role": "system", "content": "" }]);
        let out = inject(&messages, "BLOCK");
        assert_eq!(
            out.as_array().unwrap()[0]
                .get("content")
                .and_then(Value::as_str),
            Some("BLOCK")
        );
    }

    #[test]
    fn flattens_multipart_array_content_before_prepend() {
        let messages = json!([
            { "role": "user", "content": [
                { "type": "text", "text": "alpha" },
                { "type": "text", "text": "beta" }
            ]}
        ]);
        let out = inject(&messages, "BLOCK");
        assert_eq!(
            out.as_array().unwrap()[0]
                .get("content")
                .and_then(Value::as_str),
            Some("BLOCK\n\nalpha beta")
        );
    }

    #[test]
    fn empty_block_returns_input_unchanged() {
        let messages = json!([{ "role": "user", "content": "hi" }]);
        let out = inject(&messages, "");
        assert_eq!(out, messages);
    }

    #[test]
    fn preserves_remaining_messages() {
        let messages = json!([
            { "role": "user", "content": "first" },
            { "role": "assistant", "content": "second" }
        ]);
        let out = inject(&messages, "B");
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(
            arr[1].get("content").and_then(Value::as_str),
            Some("second")
        );
    }
}
