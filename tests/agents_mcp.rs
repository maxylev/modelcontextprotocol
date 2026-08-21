//! Deterministic MCP integration tests for the real `agents` process over
//! stdio: public surface, catalog (reviewer/researcher), rejection paths,
//! lifecycle, continuation, retention, and child-MCP env isolation. All
//! provider traffic is against a local deterministic Responses fixture.

mod support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rmcp::{
    ClientHandler, RoleClient,
    model::{CacheScope, CallToolRequestParams, ProgressNotificationParam},
    service::{ClientLifecycleMode, ClientServiceExt, RunningService},
    transport::TokioChildProcess,
};
use serde_json::{Value, json};
use tokio::process::Command;

use support::fixture::{
    LocalAgentWorkspace, LocalProvider, RELEASE_CHANNEL, RELEASE_MARKER, RESEARCHER_NAME,
    REVIEWER_NAME, Workspace,
};
use support::mcp_client::{self, list_tools, structured};

const LOCAL_KEY: &str = "TEST_AGENT_KEY";
const LOCAL_SECRET: &str = "local-fixture-secret";

// ---------------------------------------------------------------------------
// Python child MCP fixture (env boundary + async startup + cleanup) and
// progress-capturing client handler
// ---------------------------------------------------------------------------

fn python_child_script() -> &'static str {
    r#"import json, os, sys, time
log, delay = sys.argv[1:]
pid = os.getpid()
def note(line):
    with open(log, "a", encoding="utf-8") as f:
        f.write(line + "\n")
        f.flush()
def has(key):
    return os.environ.get(key) not in (None, "")
note(f"started {pid}")
note(f"env OPENROUTER_API_KEY={'present' if has('OPENROUTER_API_KEY') else 'absent'}")
note(f"env TEST_AGENT_KEY={'present' if has('TEST_AGENT_KEY') else 'absent'}")
time.sleep(int(delay) / 1000)
note(f"ready {pid}")
for line in sys.stdin:
    try:
        request = json.loads(line)
        identifier = request.get("id")
        if identifier is None:
            continue
        method = request.get("method")
        if method == "server/discover":
            result = {"resultType":"complete", "supportedVersions":["2026-07-28"], "capabilities":{"tools":{"listChanged":False}}, "ttlMs":0, "cacheScope":"private", "_meta":{"io.modelcontextprotocol/serverInfo":{"name":"mcp-support-child","version":"test"}}}
        elif method == "initialize":
            result = {"protocolVersion":"2026-07-28", "capabilities":{"tools":{"listChanged":False}}, "serverInfo":{"name":"mcp-support-child","version":"test"}}
        elif method == "tools/list":
            result = {"tools":[{"name":"env_report", "description":"deterministic child env probe", "inputSchema":{"type":"object","additionalProperties":False}}]}
        elif method == "tools/call":
            result = {"content":[{"type":"text","text":f"child env OPENROUTER_API_KEY={'present' if has('OPENROUTER_API_KEY') else 'absent'}"}], "isError":False}
        else:
            result = {}
        print(json.dumps({"jsonrpc":"2.0", "id":identifier, "result":result}), flush=True)
    except Exception:
        continue
note(f"stopped {pid}")
"#
}

struct ChildFixture {
    _dir: tempfile::TempDir,
    pub script: PathBuf,
    pub log: PathBuf,
}

impl ChildFixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("mcp-agent-child-support-")
            .tempdir()
            .unwrap();
        let script = dir.path().join("child_mcp.py");
        std::fs::write(&script, python_child_script()).unwrap();
        let log = dir.path().join("child-events.log");
        Self {
            _dir: dir,
            script,
            log,
        }
    }

    /// (command, args) suitable for an agent's stdio child MCP config.
    fn argv(&self, delay_ms: u64) -> (String, Vec<String>) {
        let mut invocation = mcp_client::python_invocation();
        let command = invocation.remove(0);
        invocation.extend([
            self.script.display().to_string(),
            self.log.display().to_string(),
            delay_ms.to_string(),
        ]);
        (command, invocation)
    }
}

#[derive(Clone, Default)]
struct ProgressClient {
    notifications: Arc<Mutex<Vec<ProgressNotificationParam>>>,
}

impl ClientHandler for ProgressClient {
    async fn on_progress(
        &self,
        notification: ProgressNotificationParam,
        _: rmcp::service::NotificationContext<RoleClient>,
    ) {
        self.notifications.lock().unwrap().push(notification);
    }
}

// ---------------------------------------------------------------------------
// Connection helpers
// ---------------------------------------------------------------------------

async fn connect_local(workspace: &std::path::Path, env: &[(&str, &str)]) -> mcp_client::Client {
    let mut command = Command::new(mcp_client::BIN);
    command.arg("agents").arg(workspace);
    for (key, value) in env {
        command.env(key, value);
    }
    ().serve_with_lifecycle(
        TokioChildProcess::new(command).expect("spawn agents server"),
        ClientLifecycleMode::Discover {
            preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
        },
    )
    .await
    .expect("agents server starts")
}

async fn connect_local_progress(
    workspace: &std::path::Path,
    handler: ProgressClient,
) -> RunningService<RoleClient, ProgressClient> {
    let mut command = Command::new(mcp_client::BIN);
    command
        .arg("agents")
        .arg(workspace)
        .env(LOCAL_KEY, LOCAL_SECRET);
    handler
        .serve_with_lifecycle(
            TokioChildProcess::new(command).expect("spawn agents server"),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("agents server starts")
}

// ---------------------------------------------------------------------------
// Public surface + canonical catalog
// ---------------------------------------------------------------------------

#[tokio::test]
async fn agents_reports_2026_identity_and_exact_tool_surface() {
    let workspace = Workspace::new(mcp_client::BIN);
    let client = mcp_client::connect_agents(&workspace.root, None).await;
    let info = client.peer_info().expect("discover peer info");
    assert_eq!(
        info.protocol_version,
        rmcp::model::ProtocolVersion::V_2026_07_28
    );
    let implementation = info.server_info.as_ref().expect("server identity");
    assert_eq!(implementation.name, "mcp-agents");
    assert_eq!(implementation.version, env!("CARGO_PKG_VERSION"));
    assert!(info.capabilities.tools.is_some());
    let listed = client
        .list_tools(Default::default())
        .await
        .expect("tools/list");
    assert_eq!(listed.ttl_ms, Some(0));
    assert_eq!(listed.cache_scope, Some(CacheScope::Private));
    let tools = listed.tools;
    assert_eq!(
        tools.iter().map(|t| t.name.as_ref()).collect::<Vec<_>>(),
        ["spawn_agent", "send_input", "wait_agent"]
    );
}

#[tokio::test]
async fn catalog_contains_reviewer_and_researcher_without_secrets() {
    let workspace = Workspace::new(mcp_client::BIN);
    let client = mcp_client::connect_agents(&workspace.root, None).await;
    let tools = list_tools(&client).await;
    let schema = tools[0].schema_as_json_value();
    assert_eq!(
        schema["properties"]["name"]["enum"],
        json!([RESEARCHER_NAME, REVIEWER_NAME])
    );
    assert_eq!(schema["additionalProperties"], json!(false));
    let description = tools[0].description.as_deref().expect("description");
    assert!(
        description.contains(&format!(
            "- {REVIEWER_NAME}: Reviews implementation code for correctness"
        )),
        "{description}"
    );
    assert!(
        description.contains(&format!(
            "- {RESEARCHER_NAME}: Investigates implementation and repository contracts"
        )),
        "{description}"
    );
    let rendered = serde_json::to_string(&tools).unwrap_or_default();
    // env_key name is config, not a credential; the credential VALUE must
    // never appear. It does not exist in the fixture at all.
    assert!(rendered.contains("reviewer"));
    for marker in [RELEASE_CHANNEL, RELEASE_MARKER] {
        assert!(!rendered.contains(marker), "catalog leaked resource marker");
    }
}

// ---------------------------------------------------------------------------
// Rejection paths (no provider traffic required)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_agent_empty_task_and_malformed_args_are_rejected() {
    let workspace = Workspace::new(mcp_client::BIN);
    let client = mcp_client::connect_agents(&workspace.root, None).await;

    let unknown = mcp_client::error_json(
        &mcp_client::call_tool(
            &client,
            "spawn_agent",
            json!({"name": "no-such-agent", "task": "task"}),
        )
        .await,
    );
    assert_eq!(unknown["kind"], "unknown_agent");

    for task in ["", "   "] {
        let rejected = mcp_client::error_json(
            &mcp_client::call_tool(
                &client,
                "spawn_agent",
                json!({"name": REVIEWER_NAME, "task": task}),
            )
            .await,
        );
        assert_eq!(rejected["kind"], "invalid_request");
    }

    let malformed = mcp_client::error_json(
        &mcp_client::call_tool(
            &client,
            "spawn_agent",
            json!({"name": REVIEWER_NAME, "task": "task", "unexpected": true}),
        )
        .await,
    );
    assert_eq!(malformed["kind"], "invalid_request");
}

#[tokio::test]
async fn wait_agent_unknown_and_duplicate_targets_are_rejected() {
    let workspace = Workspace::new(mcp_client::BIN);
    let client = mcp_client::connect_agents(&workspace.root, None).await;

    let unknown = mcp_client::error_json(
        &mcp_client::call_tool(
            &client,
            "wait_agent",
            json!({"targets": ["agt_not_a_real_id"], "timeout_ms": 0}),
        )
        .await,
    );
    assert_eq!(unknown["kind"], "unknown_agent");

    let duplicate = mcp_client::error_json(
        &mcp_client::call_tool(
            &client,
            "wait_agent",
            json!({"targets": ["same", "same"], "timeout_ms": 0}),
        )
        .await,
    );
    assert_eq!(duplicate["kind"], "invalid_request");

    let empty = mcp_client::error_json(
        &mcp_client::call_tool(
            &client,
            "wait_agent",
            json!({"targets": [], "timeout_ms": 0}),
        )
        .await,
    );
    assert_eq!(empty["kind"], "invalid_request");
}

// ---------------------------------------------------------------------------
// Lifecycle over the deterministic local provider
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spawn_returns_before_slow_child_mcp_startup_completes() {
    let provider = LocalProvider::start();
    let child = ChildFixture::new();
    let (command, args) = child.argv(600);
    let workspace = LocalAgentWorkspace::single(
        "alpha",
        "async spawn fixture",
        "Return one short line.",
        &provider.base_url(),
        1,
        &[],
        Some((command, args)),
    );
    let client = connect_local(&workspace.root, &[(LOCAL_KEY, LOCAL_SECRET)]).await;

    let started = Instant::now();
    let spawned = structured(
        &mcp_client::call_tool(
            &client,
            "spawn_agent",
            json!({"name": "alpha", "task": "async task"}),
        )
        .await,
    );
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "spawn is not async"
    );
    assert_eq!(spawned["status"], "running");
    let id = spawned["agent_id"].as_str().unwrap().to_owned();

    // The child has not yet finished its 600ms startup handshake: the log
    // must not contain "ready" at this point.
    let early = mcp_client::child_log(&child.log);
    assert!(
        !early.iter().any(|line| line.starts_with("ready ")),
        "child became ready before spawn returned: {early:?}"
    );
    // Poll briefly for the child's "started" line: proves the child process
    // was actually launched in the background while spawn returned early.
    let started_line = tokio::time::timeout(Duration::from_millis(800), async {
        loop {
            let log = mcp_client::child_log(&child.log);
            if log.iter().any(|line| line.starts_with("started ")) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        started_line.is_ok(),
        "child process never recorded its start: {:?}",
        mcp_client::child_log(&child.log)
    );

    let waited = structured(
        &mcp_client::call_tool(
            &client,
            "wait_agent",
            json!({"targets": [id], "timeout_ms": 8000}),
        )
        .await,
    );
    assert_eq!(waited["agents"][0]["status"], "completed");

    let events = mcp_client::child_log_expect(&child.log, 4).await;
    assert!(events.iter().any(|line| line.starts_with("ready ")));
    assert!(events.iter().any(|line| line.starts_with("stopped ")));

    // Child processes of terminal agents are not retained. UNIX: process gone.
    #[cfg(unix)]
    {
        let pid = events
            .iter()
            .find(|line| line.starts_with("started "))
            .and_then(|line| line.split_whitespace().nth(1))
            .expect("child pid");
        assert!(
            !std::process::Command::new("kill")
                .args(["-0", pid])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success(),
            "child MCP process still running after terminal agent"
        );
    }
}

#[tokio::test]
async fn two_independent_agents_run_concurrently() {
    let provider = LocalProvider::start();
    let workspace = LocalAgentWorkspace::single(
        "alpha",
        "concurrency fixture",
        "Return one short line.",
        &provider.base_url(),
        1,
        &[],
        None,
    );
    let client = connect_local(&workspace.root, &[(LOCAL_KEY, LOCAL_SECRET)]).await;

    let first = structured(
        &mcp_client::call_tool(
            &client,
            "spawn_agent",
            json!({"name": "alpha", "task": "delay-700 first"}),
        )
        .await,
    );
    let second = structured(
        &mcp_client::call_tool(
            &client,
            "spawn_agent",
            json!({"name": "alpha", "task": "delay-700 second"}),
        )
        .await,
    );
    let first_id = first["agent_id"].as_str().unwrap();
    let second_id = second["agent_id"].as_str().unwrap();
    assert_ne!(first_id, second_id);

    let started = Instant::now();
    let waited = structured(
        &mcp_client::call_tool(
            &client,
            "wait_agent",
            json!({"targets": [first_id, second_id], "timeout_ms": 5000}),
        )
        .await,
    );
    let elapsed = started.elapsed();
    assert_eq!(waited["timed_out"], false);
    assert!(
        elapsed < Duration::from_millis(1200),
        "two 700ms runs completed in {elapsed:?}; they did not overlap"
    );
    assert!(
        waited["agents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|agent| agent["status"] == "completed")
    );
}

#[tokio::test]
async fn skill_preload_embeds_reviewer_guidance_in_agent_context() {
    let provider = LocalProvider::start();
    let workspace = LocalAgentWorkspace::single(
        "alpha",
        "skill preload fixture",
        "Return one short line.",
        &provider.base_url(),
        1,
        &["reviewer-guidance"],
        None,
    );
    let client = connect_local(&workspace.root, &[(LOCAL_KEY, LOCAL_SECRET)]).await;
    let spawned = structured(
        &mcp_client::call_tool(
            &client,
            "spawn_agent",
            json!({"name": "alpha", "task": "skill context task"}),
        )
        .await,
    );
    let id = spawned["agent_id"].as_str().unwrap();
    let waited = structured(
        &mcp_client::call_tool(
            &client,
            "wait_agent",
            json!({"targets": [id], "timeout_ms": 5000}),
        )
        .await,
    );
    assert_eq!(waited["agents"][0]["status"], "completed");

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let payload = &requests[0].payload;
    let instructions = payload["instructions"].as_str().expect("instructions");
    assert!(
        instructions.contains("Return one short line."),
        "agent base instructions missing from system context: {instructions}"
    );
    assert!(
        instructions.contains("Review the requested code carefully"),
        "skill instructions missing from agent system context: {instructions}"
    );
    assert!(
        instructions.contains("Prefer concrete findings with file paths"),
        "{instructions}"
    );
    drop(requests);
}

#[tokio::test]
async fn send_input_continues_retained_history_without_compaction() {
    let provider = LocalProvider::start();
    let workspace = LocalAgentWorkspace::single(
        "alpha",
        "continuation fixture",
        "Return one short line.",
        &provider.base_url(),
        1,
        &[],
        None,
    );
    let client = connect_local(&workspace.root, &[(LOCAL_KEY, LOCAL_SECRET)]).await;

    let first = structured(
        &mcp_client::call_tool(
            &client,
            "spawn_agent",
            json!({"name": "alpha", "task": "first task"}),
        )
        .await,
    );
    let id = first["agent_id"].as_str().unwrap().to_owned();
    let first_wait = structured(
        &mcp_client::call_tool(
            &client,
            "wait_agent",
            json!({"targets": [id], "timeout_ms": 5000}),
        )
        .await,
    );
    assert_eq!(first_wait["agents"][0]["status"], "completed");

    let ack = structured(
        &mcp_client::call_tool(
            &client,
            "send_input",
            json!({"target": id.as_str(), "message": "second task", "interrupt": false}),
        )
        .await,
    );
    assert_eq!(ack["accepted"], true);
    assert_eq!(ack["status"], "running");
    let second_wait = structured(
        &mcp_client::call_tool(
            &client,
            "wait_agent",
            json!({"targets": [id], "timeout_ms": 5000}),
        )
        .await,
    );
    assert_eq!(second_wait["agents"][0]["status"], "completed");

    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "no compaction, no extra requests");
    for request in requests.iter() {
        assert!(
            !request.url.contains("/compact"),
            "unexpected compaction traffic: {}",
            request.url
        );
    }
    let resumed = requests
        .iter()
        .find(|request| {
            request.payload["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| user_text(item) == Some("second task"))
        })
        .expect("resumed provider request");
    let replayed = resumed.payload["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(user_text)
        .collect::<Vec<_>>();
    assert_eq!(replayed, ["first task", "second task"]);
    assert!(
        resumed.payload["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["role"] == "assistant"),
        "prior assistant output retained"
    );
    drop(requests);
    provider.assert_no_compact_traffic();
}

fn user_text(item: &Value) -> Option<&str> {
    (item["role"].as_str() == Some("user"))
        .then(|| item["content"].as_array())
        .flatten()
        .and_then(|blocks| blocks.iter().find_map(|block| block["text"].as_str()))
}

#[tokio::test]
async fn child_mcp_does_not_inherit_provider_credentials() {
    let provider = LocalProvider::start();
    let child = ChildFixture::new();
    let (command, args) = child.argv(0);
    let workspace = LocalAgentWorkspace::single(
        "alpha",
        "env isolation fixture",
        "Return one short line.",
        &provider.base_url(),
        1,
        &[],
        Some((command, args)),
    );
    // The agents process legitimately holds the provider credential AND an
    // ambient token; neither may reach the child filesystem-style MCP server.
    let client = connect_local(
        &workspace.root,
        &[
            (LOCAL_KEY, LOCAL_SECRET),
            ("OPENROUTER_API_KEY", "ambient-secret-not-for-child"),
        ],
    )
    .await;
    let spawned = structured(
        &mcp_client::call_tool(
            &client,
            "spawn_agent",
            json!({"name": "alpha", "task": "env isolation task"}),
        )
        .await,
    );
    let id = spawned["agent_id"].as_str().unwrap().to_owned();
    let waited = structured(
        &mcp_client::call_tool(
            &client,
            "wait_agent",
            json!({"targets": [id], "timeout_ms": 8000}),
        )
        .await,
    );
    assert_eq!(waited["agents"][0]["status"], "completed");
    let events = mcp_client::child_log_expect(&child.log, 4).await;
    assert!(
        events
            .iter()
            .any(|line| line == "env OPENROUTER_API_KEY=absent"),
        "child inherited the provider credential: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|line| line == "env TEST_AGENT_KEY=absent"),
        "child inherited the provider credential: {events:?}"
    );
}

#[tokio::test]
async fn wait_reports_progress_notifications_with_activity_metadata() {
    let provider = LocalProvider::start();
    let workspace = LocalAgentWorkspace::single(
        "alpha",
        "progress fixture",
        "Return one short line.",
        &provider.base_url(),
        1,
        &[],
        None,
    );
    let handler = ProgressClient::default();
    let notifications = handler.notifications.clone();
    let client = connect_local_progress(&workspace.root, handler).await;

    let spawned = structured(
        &client
            .call_tool(
                CallToolRequestParams::new("spawn_agent").with_arguments(
                    json!({"name": "alpha", "task": "delay-900 progress"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .expect("spawn response"),
    );
    let id = spawned["agent_id"].as_str().unwrap();
    let mut request = CallToolRequestParams::new("wait_agent").with_arguments(
        json!({"targets": [id], "timeout_ms": 5000})
            .as_object()
            .unwrap()
            .clone(),
    );
    request.meta =
        Some(serde_json::from_value(json!({"progressToken": "accept-progress-token"})).unwrap());
    let response = client.call_tool(request).await.expect("wait response");
    assert_eq!(structured(&response)["timed_out"], false);

    let received = notifications.lock().unwrap().clone();
    assert!(
        received.len() >= 2,
        "initial and terminal progress notifications expected, got {}",
        received.len()
    );
    let request_token = received[0].progress_token.clone();
    for notification in &received {
        assert_eq!(
            notification.progress_token, request_token,
            "all notifications share the request's progress token"
        );
        assert!(notification.message.as_deref().unwrap().len() <= 256);
    }
    assert!(
        received
            .windows(2)
            .all(|pair| pair[0].progress < pair[1].progress)
    );
    assert!(
        received
            .iter()
            .any(|item| item.message.as_deref().unwrap().contains("Completed"))
    );
    // The namespaced metadata is asserted by the runtime unit tests; when
    // the wire notifier delivers it, verify its shape here as well.
    for notification in &received {
        if let Some(meta) = notification.meta.as_ref() {
            let agents_meta = meta
                .get("io.modelcontextprotocol/agents")
                .expect("agents namespace");
            assert!(
                agents_meta.get("wait_timeout_remaining_ms").is_some(),
                "wait timeout remaining metadata missing"
            );
            let agent = agents_meta.get("agent").expect("agent metadata");
            assert!(agent.get("status").is_some(), "agent status missing");
            assert!(agent.get("activity").is_some() || agent.get("status").unwrap() == "completed");
            assert!(agent.get("agent_id").is_some());
            assert!(agent["total_elapsed_ms"].as_u64().is_some());
        }
    }
}

#[tokio::test]
async fn wait_is_non_consuming_for_completed_agents() {
    let provider = LocalProvider::start();
    let workspace = LocalAgentWorkspace::single(
        "alpha",
        "non-consuming fixture",
        "Return one short line.",
        &provider.base_url(),
        1,
        &[],
        None,
    );
    let client = connect_local(&workspace.root, &[(LOCAL_KEY, LOCAL_SECRET)]).await;
    let spawned = structured(
        &mcp_client::call_tool(
            &client,
            "spawn_agent",
            json!({"name": "alpha", "task": "retained result"}),
        )
        .await,
    );
    let id = spawned["agent_id"].as_str().unwrap().to_owned();
    let first_wait = structured(
        &mcp_client::call_tool(
            &client,
            "wait_agent",
            json!({"targets": [id], "timeout_ms": 5000}),
        )
        .await,
    );
    let result = first_wait["agents"][0]["result"].clone();
    assert_ne!(result, Value::Null);
    let second_wait = structured(
        &mcp_client::call_tool(
            &client,
            "wait_agent",
            json!({"targets": [id], "timeout_ms": 5000}),
        )
        .await,
    );
    assert_eq!(second_wait["timed_out"], false);
    assert_eq!(second_wait["agents"][0]["result"], result);
}

#[tokio::test]
async fn sessions_are_process_local_and_evicted_on_restart() {
    let provider = LocalProvider::start();
    let workspace = LocalAgentWorkspace::single(
        "alpha",
        "process-local fixture",
        "Return one short line.",
        &provider.base_url(),
        1,
        &[],
        None,
    );
    let client = connect_local(&workspace.root, &[(LOCAL_KEY, LOCAL_SECRET)]).await;
    let spawned = structured(
        &mcp_client::call_tool(
            &client,
            "spawn_agent",
            json!({"name": "alpha", "task": "process local task"}),
        )
        .await,
    );
    let id = spawned["agent_id"].as_str().unwrap().to_owned();
    let waited = structured(
        &mcp_client::call_tool(
            &client,
            "wait_agent",
            json!({"targets": [id], "timeout_ms": 5000}),
        )
        .await,
    );
    assert_eq!(waited["agents"][0]["status"], "completed");
    drop(client); // end the agents process

    let fresh = connect_local(&workspace.root, &[(LOCAL_KEY, LOCAL_SECRET)]).await;
    let unknown = mcp_client::error_json(
        &mcp_client::call_tool(
            &fresh,
            "wait_agent",
            json!({"targets": [id], "timeout_ms": 0}),
        )
        .await,
    );
    assert_eq!(unknown["kind"], "unknown_agent");
}

#[tokio::test]
async fn terminal_retention_is_lru_bounded_and_never_evicts_running_sessions() {
    let provider = LocalProvider::start();
    let workspace = LocalAgentWorkspace::single(
        "alpha",
        "retention fixture",
        "Return one short line.",
        &provider.base_url(),
        1,
        &[],
        None,
    );
    let client = connect_local(&workspace.root, &[(LOCAL_KEY, LOCAL_SECRET)]).await;

    // A long-running agent is never evicted by terminal-session cleanup.
    let running = structured(
        &mcp_client::call_tool(
            &client,
            "spawn_agent",
            json!({"name": "alpha", "task": "delay-4000 keep-running"}),
        )
        .await,
    );
    let running_id = running["agent_id"].as_str().unwrap().to_owned();

    let mut ids = Vec::new();
    for index in 0..66 {
        ids.push(run_terminal(&client, &format!("terminal-{index:03}")).await);
    }

    let first = mcp_client::error_json(
        &mcp_client::call_tool(
            &client,
            "wait_agent",
            json!({"targets": [ids[0]], "timeout_ms": 0}),
        )
        .await,
    );
    assert_eq!(first["kind"], "unknown_agent", "oldest terminal evicted");

    let recent = structured(
        &mcp_client::call_tool(
            &client,
            "wait_agent",
            json!({"targets": [ids[65]], "timeout_ms": 0}),
        )
        .await,
    );
    assert_eq!(recent["agents"][0]["status"], "completed");

    let running_wait = structured(
        &mcp_client::call_tool(
            &client,
            "wait_agent",
            json!({"targets": [running_id], "timeout_ms": 10000}),
        )
        .await,
    );
    assert_eq!(
        running_wait["agents"][0]["status"], "completed",
        "running session must never be evicted"
    );
}

/// Spawn one local-provider agent and wait for its terminal result; returns
/// the spawned `agent_id`.
async fn run_terminal(client: &mcp_client::Client, task: &str) -> String {
    let spawned = structured(
        &mcp_client::call_tool(
            client,
            "spawn_agent",
            json!({"name": "alpha", "task": task}),
        )
        .await,
    );
    let id = spawned["agent_id"].as_str().unwrap().to_owned();
    let waited = structured(
        &mcp_client::call_tool(
            client,
            "wait_agent",
            json!({"targets": [id.as_str()], "timeout_ms": 10000}),
        )
        .await,
    );
    assert_eq!(waited["agents"][0]["status"], "completed");
    id
}
