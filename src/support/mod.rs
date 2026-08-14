//! Neutral support shared by the concrete MCP servers.
//!
//! Dependency direction: `main`/CLI -> concrete servers (`fs`, `fetch`,
//! `memory`, `shell`) -> this module -> external crates. Concrete servers
//! never depend on one another; everything they share lives here.

mod access;

pub use access::AccessControl;

use rmcp::model::{CallToolResult, ContentBlock};

/// The MCP protocol version implemented by every server in this binary.
pub const SPEC_VERSION: &str = "2026-07-28";

/// A tool-level error result carrying a plain-text message.
pub fn tool_error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// A successful tool result carrying plain text content.
pub fn text_result(text: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text)])
}
