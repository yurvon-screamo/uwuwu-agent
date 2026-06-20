//! `HandleChatCompletion` — top-level chat-completion use case (§3 + §4).
//!
//! Orchestrates the full request-side pipeline:
//! 1. Parse `"memory_key:real_model"` and strip the prefix.
//! 2. Detect the session id from the trailing 20 messages' markers (or mint a
//!    fresh one).
//! 3. Run [`EnrichRequest`] (memory retrieval + injection). Infallible: the
//!    use case's fail-open contract is enforced at the type level, so the
//!    original `messages` are always preserved (the request is forwarded
//!    unchanged if any enrichment port misbehaves).
//! 4. Forward the (possibly enriched) request to the LLM upstream.
//!
//! Slice-5 extraction is wired in the **adapter** layer (`http/`), not here.
//! The application layer stays runtime-agnostic: `tokio::spawn` requires a
//! multi-thread runtime, and the SMOS codebase keeps every runtime operation
//! (spawn, serve, signal handling) inside `smos-adapters`. The adapter wraps
//! the response stream with a `StreamingBuffer`, and after `[DONE]` spawns the
//! [`ExtractFactsFromResponse`] use case. This use case hands the adapter the
//! `MemoryKey` it needs for that wiring.
//!
//! Returns `(ChatResponse, SessionId, MemoryKey)` so the HTTP handler injects
//! the session marker AND the adapter wires extraction with the right project.

use std::sync::Arc;

use serde_json::Value;
use smos_domain::chat::ToolCall;
use smos_domain::config::{HeatConfig, RetrievalConfig};
use smos_domain::{MemoryKey, SessionId};

use crate::errors::UseCaseError;
use crate::helpers::{model_parser, session_marker};
use crate::ports::{
    Clock, EmbeddingProvider, FactRepository, LlmUpstream, RerankProvider, SessionRepository,
};
use crate::types::{ChatRequest, ChatResponse};
use crate::use_cases::enrich_request::EnrichRequest;

/// Top-level chat-completion orchestration.
///
/// Owns the ports the REQUEST-side pipeline needs (enrichment + upstream
/// forwarding). Extraction ports live in `AppState` and are wired by the
/// adapter — see the module docs for the layering rationale.
pub struct HandleChatCompletion<FR, SR, EP, RP, LU, C> {
    pub facts: FR,
    pub sessions: SR,
    pub embedder: EP,
    pub reranker: RP,
    pub upstream: LU,
    pub clock: C,
    pub retrieval_cfg: Arc<RetrievalConfig>,
    pub heat_cfg: Arc<HeatConfig>,
}

impl<FR, SR, EP, RP, LU, C> HandleChatCompletion<FR, SR, EP, RP, LU, C>
where
    FR: FactRepository,
    SR: SessionRepository,
    EP: EmbeddingProvider,
    RP: RerankProvider,
    LU: LlmUpstream,
    C: Clock,
{
    /// Run the chat-completion pipeline.
    ///
    /// Returns the upstream response, the session id (so the handler injects
    /// the marker), and the memory namespace (so the adapter spawns the
    /// extraction task against the correct project).
    pub async fn execute(
        &self,
        mut request: ChatRequest,
    ) -> Result<(ChatResponse, SessionId, MemoryKey), UseCaseError> {
        // Step 1 — parse model.
        let (memory_key, real_model) = model_parser::parse_model(&request.model)?;
        request.model = real_model;

        // Step 2 — detect session.
        let session_id =
            session_marker::detect_from_messages(&request.messages).unwrap_or_default();

        // Step 3 — enrichment (infallible fail-open). `EnrichRequest::execute`
        // returns `Vec<Value>` directly: every port-level error already
        // fail-opens to the original messages inside `execute`, so the type
        // system rules out a future bug where an `Err` arm silently replaces
        // the consumed `request.messages` with `Vec::new()`. We `mem::take`
        // because the result is guaranteed to contain at least the original
        // messages — no extra clone, no copy-back risk.
        let enriched_messages = self
            .enrich(
                std::mem::take(&mut request.messages),
                &memory_key,
                &session_id,
            )
            .await;
        request.messages = enriched_messages;

        // Step 4 — forward.
        let response = self.upstream.complete(request).await?;
        Ok((response, session_id, memory_key))
    }

    /// Run `EnrichRequest` and return its result. With the infallible
    /// `execute` signature there is no error to swallow — the §12 fail-open
    /// contract is enforced inside `EnrichRequest::execute` (every port-level
    /// error falls back to the original messages), so this is a thin wrapper
    /// that exists only to keep `execute` readable.
    async fn enrich(
        &self,
        messages: Vec<Value>,
        memory_key: &MemoryKey,
        session_id: &SessionId,
    ) -> Vec<Value> {
        let enrich = EnrichRequest {
            facts: &self.facts,
            sessions: &self.sessions,
            embedder: &self.embedder,
            reranker: &self.reranker,
            clock: &self.clock,
            retrieval_cfg: &self.retrieval_cfg,
            heat_cfg: &self.heat_cfg,
        };
        enrich.execute(messages, memory_key, session_id).await
    }
}

/// Extract the assistant content + structured tool calls from an OpenAI-shaped
/// non-streaming response so the extraction pipeline can reason over both.
///
/// `arguments` arrives as a JSON **string** on the wire (OpenAI quirk); it is
/// parsed into a `Value` for the domain [`ToolCall`]. Unparseable argument
/// strings fall back to the raw string so no information is lost. Exported so
/// the adapter can run the same parsing on the buffered non-streaming body
/// before spawning the background extraction task.
pub fn extract_response_payload(value: &Value) -> (String, Vec<ToolCall>) {
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let tool_calls = value
        .pointer("/choices/0/message/tool_calls")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(parse_openai_tool_call).collect())
        .unwrap_or_default();
    (content, tool_calls)
}

/// Convert one OpenAI tool-call object (`{id, type, function:{name, arguments}}`)
/// into the domain [`ToolCall`] shape.
fn parse_openai_tool_call(v: &Value) -> Option<ToolCall> {
    let function = v.get("function")?;
    let name = function.get("name")?.as_str()?.to_string();
    let arguments = match function.get("arguments") {
        Some(Value::String(raw)) => {
            serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.clone()))
        }
        Some(other) => other.clone(),
        None => Value::Null,
    };
    Some(ToolCall { name, arguments })
}
