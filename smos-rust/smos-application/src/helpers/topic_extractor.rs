//! Topic extractor — flatten the last chat message's content (§3 step 3).
//!
//! Thin wrapper around [`crate::helpers::openai_content::flatten_text`] kept as
//! a named entry point so the call sites in `EnrichRequest` read as
//! "extract_topic" rather than "flatten content". Null / missing content
//! degrades to an empty string — the caller filters on length.

use serde_json::Value;

use crate::helpers::openai_content::flatten_text;

/// Flatten a JSON-encoded message content into a single topic string.
///
/// - String content → returned verbatim.
/// - Array of `{type: "text", text: "..."}` parts → joined with `" "`.
/// - Anything else (null, number, object, missing) → empty string.
pub fn extract_from_content(content: &Value) -> String {
    flatten_text(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plain_string_content_is_returned_verbatim() {
        assert_eq!(extract_from_content(&json!("hello world")), "hello world");
    }

    #[test]
    fn array_of_text_parts_is_joined_with_space() {
        let content = json!([
            { "type": "text", "text": "hello" },
            { "type": "text", "text": "world" }
        ]);
        assert_eq!(extract_from_content(&content), "hello world");
    }

    #[test]
    fn non_text_parts_are_skipped() {
        let content = json!([
            { "type": "image_url", "image_url": "..." },
            { "type": "text", "text": "keep me" }
        ]);
        assert_eq!(extract_from_content(&content), "keep me");
    }

    #[test]
    fn null_content_yields_empty_string() {
        assert_eq!(extract_from_content(&Value::Null), "");
    }

    #[test]
    fn missing_content_field_yields_empty_string_when_undefined() {
        let msg = json!({ "role": "user" });
        let content = msg.get("content").unwrap_or(&Value::Null);
        assert_eq!(extract_from_content(content), "");
    }

    #[test]
    fn empty_text_array_yields_empty_string() {
        assert_eq!(extract_from_content(&json!([])), "");
    }

    #[test]
    fn number_content_yields_empty_string() {
        assert_eq!(extract_from_content(&json!(42)), "");
    }
}
