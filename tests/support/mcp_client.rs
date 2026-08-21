//! Spawn and drive the real `modelcontextprotocol` binary over the real MCP
//! stdio transport using the 2026-07-28 Discover lifecycle, plus event-log
//! helpers for the deterministic child-MCP fixtures used by lifecycle tests.

use std::path::Path;
use std::time::Duration;

use rmcp::{
    RoleClient,
    model::{CallToolRequestParams, CallToolResult, ContentBlock, Tool},
    service::{ClientLifecycleMode, ClientServiceExt, RunningService},
    transport::TokioChildProcess,
};
use serde_json::Value;
use tokio::process::Command;

pub type Client = RunningService<RoleClient, ()>;

pub const BIN: &str = env!("CARGO_BIN_EXE_modelcontextprotocol");

pub fn spawn_client(args: &[&str], env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(BIN);
    command.args(args);
    for (key, value) in env {
        command.env(key, value);
    }
    command
}

/// Spawn one real server over stdio and connect with the 2026-07-28 Discover
/// lifecycle. Returns the running client.
pub async fn connect(args: &[&str], env: &[(&str, &str)]) -> Client {
    connect_with_command(spawn_client(args, env)).await
}

pub async fn connect_with_command(mut command: Command) -> Client {
    command.kill_on_drop(true);
    ().serve_with_lifecycle(
        TokioChildProcess::new(command).expect("spawn server"),
        ClientLifecycleMode::Discover {
            preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
        },
    )
    .await
    .expect("server starts via MCP 2026-07-28 Discover")
}

pub async fn connect_skills(workspace: &Path, with_key: Option<&str>) -> Client {
    let env: Vec<(&str, &str)> = with_key
        .map(|key| vec![("OPENROUTER_API_KEY", key)])
        .unwrap_or_default();
    connect(&["skills", &workspace.display().to_string()], &env).await
}

pub async fn connect_agents(workspace: &Path, key: Option<&str>) -> Client {
    let env: Vec<(&str, &str)> = key
        .map(|key| vec![("OPENROUTER_API_KEY", key)])
        .unwrap_or_default();
    connect(&["agents", &workspace.display().to_string()], &env).await
}

pub async fn connect_filesystem(workspace: &Path, with_key: Option<&str>) -> Client {
    let env: Vec<(&str, &str)> = with_key
        .map(|key| vec![("OPENROUTER_API_KEY", key)])
        .unwrap_or_default();
    connect(&["filesystem", &workspace.display().to_string()], &env).await
}

pub async fn list_tools(client: &Client) -> Vec<Tool> {
    client.list_all_tools().await.expect("tools/list over MCP")
}

pub async fn list_tool_names(client: &Client) -> Vec<String> {
    list_tools(client)
        .await
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect()
}

pub async fn call_tool(client: &Client, name: &str, args: Value) -> CallToolResult {
    client
        .call_tool(
            CallToolRequestParams::new(name.to_string())
                .with_arguments(args.as_object().expect("arguments object").clone()),
        )
        .await
        .expect("tools/call over MCP")
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

/// Structured content of a tool result (panics if absent).
pub fn structured(result: &CallToolResult) -> Value {
    result
        .structured_content
        .clone()
        .expect("structured content")
}

pub fn error_json(result: &CallToolResult) -> Value {
    assert!(
        result.is_error.unwrap_or(false),
        "expected a tool-level error"
    );
    serde_json::from_str(&text(result)).expect("error JSON")
}

/// Return the discovered implementation name for a connected client.
pub fn identity_name(client: &Client) -> String {
    client
        .peer_info()
        .and_then(|info| {
            info.server_info
                .as_ref()
                .map(|server| server.name.to_string())
        })
        .expect("server identity")
}

// ---------------------------------------------------------------------------
// Deterministic child-MCP event log helpers
// ---------------------------------------------------------------------------

/// Read the deterministic child log lines.
pub fn child_log(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Wait until the requested number of lines are present in the child log.
pub async fn child_log_expect(path: &Path, expected: usize) -> Vec<String> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let lines = child_log(path);
            if lines.len() >= expected {
                return lines;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "child log did not reach {expected} lines: {}",
            std::fs::read_to_string(path).unwrap_or_default()
        )
    })
}
