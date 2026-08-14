//! Shared helpers for the end-to-end MCP server test suites.
//!
//! Each server test file keeps its own `connect`/`run_test` (connection
//! arguments and timeouts differ per server) and uses the common protocol
//! helpers below.

use rmcp::{
    RoleClient,
    model::{CallToolRequestParams, CallToolResult, ContentBlock},
    service::RunningService,
};

pub type Client = RunningService<RoleClient, ()>;

/// Call a tool and unwrap the protocol-level result.
pub async fn call_tool(client: &Client, name: &str, args: serde_json::Value) -> CallToolResult {
    client
        .call_tool(
            CallToolRequestParams::new(name.to_string())
                .with_arguments(args.as_object().expect("args object").clone()),
        )
        .await
        .expect("tool call succeeds")
}

/// Concatenated text content of a tool result.
pub fn text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}
