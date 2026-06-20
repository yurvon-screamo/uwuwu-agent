//! `StreamingBuffer` — concurrent accumulator for the streaming extraction
//! path (§4 + §5).
//!
//! The streaming passthrough forwards every upstream chunk to the client 1:1
//! (with the session marker appended to the terminal chunk). Slice-5 ALSO
//! copies `content` + reassembled `tool_calls` into this buffer so the
//! post-`[DONE]` extraction prompt sees the full reply.
//!
//! OpenAI streams `tool_calls` incrementally by `index`: the first delta
//! carries `function.name`, subsequent deltas append to
//! `function.arguments` (a JSON **string** split across chunks). The buffer
//! reassembles them positionally and parses the arguments string into a
//! [`ToolCall`] on [`StreamingBuffer::finalize`].

use std::sync::Arc;

use smos_domain::chat::ToolCall;
use tokio::sync::Mutex;

/// Concurrent buffer that accumulates `content` + `tool_calls` from a
/// streaming response. Cheap to clone (one `Arc`); the clone shares the same
/// underlying state.
#[derive(Clone, Default)]
pub struct StreamingBuffer {
    inner: Arc<Mutex<BufferState>>,
}

#[derive(Default)]
struct BufferState {
    content: String,
    tool_calls: Vec<PartialToolCall>,
}

/// In-progress tool call before its arguments JSON is complete.
#[derive(Default)]
struct PartialToolCall {
    name: String,
    args: String,
}

impl StreamingBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a content delta (SSE `choices[0].delta.content`).
    pub async fn append_content(&self, delta: &str) {
        self.inner.lock().await.content.push_str(delta);
    }

    /// Append a tool-call delta for the tool call at `index`. SSE chunks arrive
    /// incrementally: the first delta usually carries `function.name`, later
    /// deltas append to `function.arguments`. Missing deltas (`None`) leave
    /// the field untouched.
    pub async fn append_tool_call_delta(
        &self,
        index: usize,
        name_delta: Option<&str>,
        args_delta: Option<&str>,
    ) {
        let mut state = self.inner.lock().await;
        while state.tool_calls.len() <= index {
            state.tool_calls.push(PartialToolCall::default());
        }
        let slot = &mut state.tool_calls[index];
        if let Some(name) = name_delta {
            slot.name.push_str(name);
        }
        if let Some(args) = args_delta {
            slot.args.push_str(args);
        }
    }

    /// Drain the accumulated content + reassembled tool calls. Each tool
    /// call's arguments string is parsed into a [`serde_json::Value`]; an
    /// unparseable arguments string degrades to the raw string so no
    /// information is lost.
    pub async fn finalize(self) -> (String, Vec<ToolCall>) {
        let mut state = self.inner.lock().await;
        let content = std::mem::take(&mut state.content);
        let partials = std::mem::take(&mut state.tool_calls);
        let tool_calls = partials
            .into_iter()
            .map(|p| ToolCall {
                name: p.name,
                arguments: serde_json::from_str(&p.args)
                    .unwrap_or_else(|_| serde_json::Value::String(p.args)),
            })
            .collect();
        (content, tool_calls)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn append_content_concatenates_deltas() {
        let buf = StreamingBuffer::new();
        buf.append_content("Hello ").await;
        buf.append_content("world").await;
        let (content, calls) = buf.finalize().await;
        assert_eq!(content, "Hello world");
        assert!(calls.is_empty());
    }

    #[tokio::test]
    async fn append_tool_call_assembles_name_then_arguments() {
        let buf = StreamingBuffer::new();
        // First delta: name only.
        buf.append_tool_call_delta(0, Some("read_file"), None).await;
        // Second delta: arguments fragment.
        buf.append_tool_call_delta(0, None, Some("{\"path\":\"a"))
            .await;
        // Third delta: arguments tail.
        buf.append_tool_call_delta(0, None, Some("uth.rs\"}")).await;

        let (_content, calls) = buf.finalize().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments["path"], "auth.rs");
    }

    #[tokio::test]
    async fn append_tool_call_handles_multiple_indices_in_order() {
        let buf = StreamingBuffer::new();
        buf.append_tool_call_delta(0, Some("first"), None).await;
        buf.append_tool_call_delta(1, Some("second"), Some("{}"))
            .await;
        buf.append_tool_call_delta(0, None, Some("{\"x\":1}")).await;

        let (_content, calls) = buf.finalize().await;
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "first");
        assert_eq!(calls[0].arguments["x"], 1);
        assert_eq!(calls[1].name, "second");
    }

    #[tokio::test]
    async fn finalize_keeps_raw_arguments_when_unparseable() {
        let buf = StreamingBuffer::new();
        buf.append_tool_call_delta(0, Some("x"), Some("{not json"))
            .await;
        let (_content, calls) = buf.finalize().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, serde_json::json!("{not json"));
    }

    #[tokio::test]
    async fn empty_buffer_finalizes_to_empty() {
        let buf = StreamingBuffer::new();
        let (content, calls) = buf.finalize().await;
        assert!(content.is_empty());
        assert!(calls.is_empty());
    }

    #[tokio::test]
    async fn clone_shares_state_between_handles() {
        let buf = StreamingBuffer::new();
        let clone = buf.clone();
        clone.append_content("shared").await;
        let (content, _calls) = buf.finalize().await;
        assert_eq!(content, "shared", "clone shares the underlying state");
    }
}
