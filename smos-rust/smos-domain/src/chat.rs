//! Pure chat message types.
//!
//! `ChatRequest` lives in the application layer (slice 3): it carries the full
//! OpenAI-compatible envelope. The domain layer only needs the message and
//! tool-call shapes so it can reason about content manipulation (enrichment,
//! marker detection, topic extraction).

use serde::{Deserialize, Serialize};

/// A single tool invocation reported by the assistant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    /// Tool-specific payload (kept opaque at this layer).
    pub arguments: serde_json::Value,
}

/// One turn in a chat conversation.
///
/// `tool_calls` is omitted from the wire format when empty so round-tripped
/// messages stay close to the OpenAI shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
}

impl ChatMessage {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tool_calls: Vec::new(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new("user", content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new("assistant", content)
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::new("system", content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_has_user_role() {
        let m = ChatMessage::user("hello");
        assert_eq!(m.role, "user");
        assert_eq!(m.content, "hello");
        assert!(m.tool_calls.is_empty());
    }

    #[test]
    fn serde_roundtrip_preserves_simple_message() {
        let m = ChatMessage::system("be helpful");
        let json = serde_json::to_string(&m).unwrap();
        let back: ChatMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn serde_omits_empty_tool_calls() {
        let m = ChatMessage::assistant("ok");
        let json = serde_json::to_string(&m).unwrap();
        assert!(!json.contains("tool_calls"));
    }

    #[test]
    fn serde_preserves_tool_calls_when_present() {
        let m = ChatMessage {
            role: "assistant".to_string(),
            content: "running it".to_string(),
            tool_calls: vec![ToolCall {
                name: "bash".to_string(),
                arguments: serde_json::json!({"cmd": "ls"}),
            }],
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: ChatMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
        assert_eq!(back.tool_calls.len(), 1);
    }

    #[test]
    fn serde_default_fills_missing_tool_calls() {
        let json = r#"{"role":"user","content":"hi"}"#;
        let back: ChatMessage = serde_json::from_str(json).unwrap();
        assert_eq!(back.role, "user");
        assert!(back.tool_calls.is_empty());
    }
}
