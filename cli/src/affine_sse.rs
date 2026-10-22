//! SSE payload extraction for the MCP Streamable HTTP transport.
//!
//! Follows the SSE event format: an event is a block of lines, multiple
//! `data:` lines of one event concatenate with `\n` (per the WHATWG spec).
//! The response for a request carries the request id — we match it instead
//! of blindly taking the last event.

use serde_json::Value;

/// Extract the JSON payload for `expected_id` from either a plain JSON body
/// or an SSE stream. Notifications (no id) answer with an empty body and
/// must not reach this function.
pub fn parse_response(raw: &str, expected_id: u64) -> Result<Value, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty body".to_string());
    }
    if trimmed.starts_with('{') {
        let value: Value = serde_json::from_str(trimmed).map_err(|e| e.to_string())?;
        return match value.get("id").and_then(|v| v.as_u64()) {
            Some(id) if id == expected_id => Ok(value),
            Some(id) => Err(format!("id mismatch: expected {expected_id}, got {id}")),
            None => Err("response carries no id".to_string()),
        };
    }
    for event in sse_events(trimmed) {
        let Ok(value) = serde_json::from_str::<Value>(&event) else {
            continue;
        };
        match value.get("id").and_then(|v| v.as_u64()) {
            Some(id) if id == expected_id => return Ok(value),
            _ => continue,
        }
    }
    Err(format!("no SSE event with id {expected_id}"))
}

/// Rebuild each SSE event's data payload: lines `data: x` within one event
/// (separated by a blank line) join with `\n`.
fn sse_events(raw: &str) -> Vec<String> {
    let mut events = Vec::new();
    let mut data_lines: Vec<String> = Vec::new();
    for line in raw.lines() {
        let trimmed_end = line.trim_end_matches('\r');
        if trimmed_end.is_empty() {
            if !data_lines.is_empty() {
                events.push(data_lines.join("\n"));
                data_lines.clear();
            }
            continue;
        }
        if let Some(data) = trimmed_end.strip_prefix("data:") {
            data_lines.push(data.strip_prefix(' ').unwrap_or(data).to_string());
        }
    }
    if !data_lines.is_empty() {
        events.push(data_lines.join("\n"));
    }
    events
}
