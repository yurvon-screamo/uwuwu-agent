//! HTTP/MCP-presentation: health/статус, JSON-RPC streamable HTTP, инструменты.

mod http;
mod mcp;
mod tools;

pub use http::{router as http_router, AppState};
pub use mcp::{router as mcp_router, McpState};
pub use tools::ToolHub;
