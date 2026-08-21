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

/// Locate a Python interpreter usable for the stdio child-MCP fixtures.
/// Prefers `python3`; falls back to `python` and `py -3` because the GitHub
/// Actions Windows runners do not put `python3` on PATH. Returns the argv
/// prefix (command plus any fixed arguments such as `py -3`).
///
/// Integration tests compile this module independently, so not every crate uses it.
#[allow(dead_code)]
pub fn python_invocation() -> Vec<String> {
    const CANDIDATES: &[&[&str]] = &[&["python3"], &["python"], &["py", "-3"]];
    for candidate in CANDIDATES {
        let probe = std::process::Command::new(candidate[0])
            .args(&candidate[1..])
            .arg("--version")
            .output();
        if probe.map(|out| out.status.success()).unwrap_or(false) {
            return candidate.iter().map(|s| s.to_string()).collect();
        }
    }
    panic!("no Python interpreter (python3/python/py) found for child MCP fixtures")
}

/// Concatenated text content of a tool result.
///
/// Integration tests compile this module independently, so not every crate uses it.
#[allow(dead_code)]
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
