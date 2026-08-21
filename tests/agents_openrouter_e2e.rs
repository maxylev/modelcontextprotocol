//! Ignored real-network coverage for the agents runtime through OpenRouter.

mod common;

use std::time::Duration;

use common::{Client, call_tool};
use rmcp::{
    service::{ClientLifecycleMode, ClientServiceExt},
    transport::TokioChildProcess,
};
use serde::Serialize;
use serde_json::json;
use tempfile::TempDir;
use tokio::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_modelcontextprotocol");
const WHOLE_TEST_TIMEOUT: Duration = Duration::from_secs(180);
const WAIT_TIMEOUT_MS: u64 = 120_000;

#[derive(Serialize)]
struct OpenRouterAgentToml<'a> {
    name: &'a str,
    description: &'a str,
    instructions: &'a str,
    model: &'a str,
    model_provider: &'a str,
    base_url: &'a str,
    env_key: &'a str,
    wire_api: &'a str,
    max_turns: u8,
}

fn required_env(name: &str) -> String {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => panic!(
            "agents_openrouter_e2e requires the {name} environment variable; \
             source it in the caller before running this ignored test"
        ),
    }
}

fn workspace(model: &str, endpoint: &str) -> TempDir {
    let root = tempfile::Builder::new()
        .prefix("mcp-agents-openrouter-")
        .tempdir()
        .unwrap_or_else(|_| panic!("create temporary workspace"));
    let agents = root.path().join(".agents/agents");
    std::fs::create_dir_all(&agents).unwrap_or_else(|_| panic!("create agent directory"));

    // toml serialization, rather than string interpolation, keeps caller
    // supplied model and endpoint values confined to correctly quoted fields.
    let definition = toml::to_string(&OpenRouterAgentToml {
        name: "openrouter",
        description: "OpenRouter Responses acceptance agent",
        instructions: "Return one short final response. Do not use tools.",
        model,
        model_provider: "custom",
        base_url: endpoint,
        env_key: "OPENROUTER_API_KEY",
        wire_api: "responses",
        max_turns: 1,
    })
    .unwrap_or_else(|_| panic!("serialize agent TOML"));
    std::fs::write(agents.join("openrouter.toml"), definition)
        .unwrap_or_else(|_| panic!("write agent TOML"));
    root
}

async fn connect(workspace: &std::path::Path, api_key: &str) -> Client {
    let mut command = Command::new(BIN);
    command
        .arg("agents")
        .arg(workspace)
        // The server needs no ambient variables for this test. This also
        // prevents unrelated provider credentials reaching the child.
        .env_clear()
        .env("OPENROUTER_API_KEY", api_key);
    ().serve_with_lifecycle(
        TokioChildProcess::new(command).unwrap_or_else(|_| panic!("spawn agents server")),
        ClientLifecycleMode::Discover {
            preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
        },
    )
    .await
    .unwrap_or_else(|_| panic!("initialize agents server"))
}

fn structured(result: &rmcp::model::CallToolResult) -> serde_json::Value {
    assert!(
        !result.is_error.unwrap_or(false),
        "agents tool returned an error"
    );
    result
        .structured_content
        .clone()
        .unwrap_or_else(|| panic!("agents tool returned no structured content"))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "real-network test; requires OPENROUTER_API_KEY, OPENROUTER_MODEL, and OPENROUTER_ENDPOINT"]
async fn agents_openrouter_responses_e2e() {
    let api_key = required_env("OPENROUTER_API_KEY");
    let model = required_env("OPENROUTER_MODEL");
    let endpoint = required_env("OPENROUTER_ENDPOINT");

    tokio::time::timeout(WHOLE_TEST_TIMEOUT, async {
        let root = workspace(&model, &endpoint);
        let client = connect(root.path(), &api_key).await;
        assert_eq!(
            client
                .peer_info()
                .and_then(|info| info
                    .server_info
                    .as_ref()
                    .map(|server| server.name.to_string()))
                .as_deref()
                .unwrap_or(""),
            "mcp-agents",
            "Discover did not identify mcp-agents"
        );

        let spawned = structured(
            &call_tool(
                &client,
                "spawn_agent",
                json!({
                    "name": "openrouter",
                    "task": "Reply with exactly OK. Do not call any tools."
                }),
            )
            .await,
        );
        assert_eq!(spawned["status"], "running");
        assert!(
            spawned.get("result").is_none(),
            "spawn unexpectedly returned a final result"
        );
        let agent_id = spawned["agent_id"]
            .as_str()
            .unwrap_or_else(|| panic!("spawn returned no agent identifier"));

        let waited = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets": [agent_id], "timeout_ms": WAIT_TIMEOUT_MS}),
            )
            .await,
        );
        assert_eq!(waited["timed_out"], false, "agent wait timed out");
        assert_eq!(waited["agents"][0]["status"], "completed");
        assert!(
            waited["agents"][0]["result"]
                .as_str()
                .is_some_and(|result| !result.trim().is_empty()),
            "completed agent returned an empty result"
        );
    })
    .await
    .unwrap_or_else(|_| panic!("agents OpenRouter test exceeded 180 seconds"));
}
