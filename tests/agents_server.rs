//! End-to-end coverage for the local agents server and its Responses provider.

mod common;

use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use common::{Client, call_tool, python_invocation, text};
use rmcp::{
    ClientHandler, RoleClient,
    model::{CallToolRequestParams, ProgressNotificationParam},
    service::{ClientLifecycleMode, ClientServiceExt},
    transport::TokioChildProcess,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tiny_http::{Header, Response, Server, StatusCode};
use tokio::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_modelcontextprotocol");
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

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

/// A small, real HTTP Responses endpoint.  Each accepted HTTP request is
/// handled independently, which deliberately exercises concurrent provider
/// calls rather than simulating the provider inside the server process.
struct ResponsesFixture {
    port: u16,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    alive: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

#[derive(Clone)]
struct CapturedRequest {
    authorization: Option<String>,
    payload: Value,
}

impl ResponsesFixture {
    fn start() -> Self {
        let server = Server::http("127.0.0.1:0").expect("start local provider");
        let port = server.server_addr().to_ip().expect("IP listener").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let captured = requests.clone();
        let running = alive.clone();
        let worker = thread::spawn(move || {
            while running.load(Ordering::Acquire) {
                let Ok(Some(mut request)) = server.recv_timeout(Duration::from_millis(25)) else {
                    continue;
                };
                let captured = captured.clone();
                thread::spawn(move || {
                    let method = request.method().as_str().to_owned();
                    let url = request.url().to_owned();
                    let mut body = String::new();
                    request
                        .as_reader()
                        .read_to_string(&mut body)
                        .expect("read provider request");
                    let payload: Value = serde_json::from_str(&body).expect("Responses JSON");
                    let authorization = request
                        .headers()
                        .iter()
                        .find(|header| {
                            header
                                .field
                                .to_string()
                                .eq_ignore_ascii_case("authorization")
                        })
                        .map(|header| header.value.as_str().to_owned());
                    captured.lock().expect("request log").push(CapturedRequest {
                        authorization,
                        payload: payload.clone(),
                    });
                    assert_eq!(method, "POST");
                    assert_eq!(url, "/v1/responses");

                    let input = payload["input"].as_array().expect("replayed input array");
                    let latest = input
                        .iter()
                        .rev()
                        .find_map(user_text)
                        .expect("delegated user task");
                    if latest.contains("delay-200") {
                        thread::sleep(Duration::from_millis(200));
                    }
                    if latest.contains("delay-1000") {
                        thread::sleep(Duration::from_secs(1));
                    }
                    if latest.contains("context-limit") {
                        let response = Response::from_string(
                            json!({"error":{"code":"context_length_exceeded","message":"secret provider payload"}})
                                .to_string(),
                        )
                        .with_status_code(StatusCode(400))
                        .with_header(
                            Header::from_bytes("content-type", "application/json").unwrap(),
                        );
                        let _ = request.respond(response);
                        return;
                    }
                    let answer = format!("provider result: {latest}");
                    let response = json!({
                        "output": [{"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":answer}]}],
                        "output_text": answer,
                    });
                    let response = Response::from_string(response.to_string())
                        .with_status_code(StatusCode(200))
                        .with_header(
                            Header::from_bytes("content-type", "application/json").unwrap(),
                        );
                    // A cancelled agent may close its provider connection before
                    // this deliberately delayed fixture responds.
                    let _ = request.respond(response);
                });
            }
        });
        Self {
            port,
            requests,
            alive,
            worker: Some(worker),
        }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }
}

impl Drop for ResponsesFixture {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        self.worker
            .take()
            .expect("fixture worker")
            .join()
            .expect("stop fixture");
    }
}

/// Two real loopback origins used to prove the provider client never follows a
/// redirect carrying a bearer credential to another origin.
struct RedirectFixture {
    base_url: String,
    redirected_requests: Arc<Mutex<usize>>,
    alive: Arc<AtomicBool>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl RedirectFixture {
    fn start() -> Self {
        let second = Server::http("127.0.0.1:0").unwrap();
        let second_port = second.server_addr().to_ip().unwrap().port();
        let first = Server::http("127.0.0.1:0").unwrap();
        let first_port = first.server_addr().to_ip().unwrap().port();
        let alive = Arc::new(AtomicBool::new(true));
        let redirected_requests = Arc::new(Mutex::new(0));
        let first_alive = alive.clone();
        let redirect = format!("http://127.0.0.1:{second_port}/v1/responses");
        let first_worker = thread::spawn(move || {
            while first_alive.load(Ordering::Acquire) {
                let Ok(Some(request)) = first.recv_timeout(Duration::from_millis(25)) else {
                    continue;
                };
                let response = Response::empty(StatusCode(302))
                    .with_header(Header::from_bytes("location", redirect.as_bytes()).unwrap());
                let _ = request.respond(response);
            }
        });
        let second_alive = alive.clone();
        let hits = redirected_requests.clone();
        let second_worker = thread::spawn(move || {
            while second_alive.load(Ordering::Acquire) {
                let Ok(Some(request)) = second.recv_timeout(Duration::from_millis(25)) else {
                    continue;
                };
                *hits.lock().unwrap() += 1;
                let _ = request.respond(Response::empty(StatusCode(200)));
            }
        });
        Self {
            base_url: format!("http://127.0.0.1:{first_port}/v1"),
            redirected_requests,
            alive,
            workers: vec![first_worker, second_worker],
        }
    }
}

impl Drop for RedirectFixture {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        for worker in self.workers.drain(..) {
            worker.join().unwrap();
        }
    }
}

fn user_text(value: &Value) -> Option<&str> {
    (value["role"].as_str() == Some("user"))
        .then(|| value["content"].as_array())
        .flatten()
        .and_then(|blocks| blocks.iter().find_map(|b| b["text"].as_str()))
}

fn workspace(provider: &ResponsesFixture) -> TempDir {
    let root = tempfile::Builder::new()
        .prefix("mcp-agents-test-")
        .tempdir()
        .unwrap();
    let agents = root.path().join(".agents/agents");
    std::fs::create_dir_all(&agents).unwrap();
    let agent = |name: &str, description: &str| {
        format!(
            "name = \"{name}\"\ndescription = \"{description}\"\ninstructions = \"Private instruction: never expose this.\"\nmodel = \"fixture-model\"\nmodel_provider = \"custom\"\nbase_url = \"{}\"\nenv_key = \"TEST_AGENT_KEY\"\nwire_api = \"responses\"\n",
            provider.base_url()
        )
    };
    // Deliberately reverse file order: discovery must sort by definition name.
    std::fs::write(
        agents.join("zeta.toml"),
        agent("zeta", "Second fixture agent"),
    )
    .unwrap();
    std::fs::write(
        agents.join("alpha.toml"),
        agent("alpha", "First fixture agent"),
    )
    .unwrap();
    root
}

fn child_mcp_workspace(provider: &ResponsesFixture, log: &Path) -> TempDir {
    let root = tempfile::Builder::new()
        .prefix("mcp-agent-child-lifecycle-")
        .tempdir()
        .unwrap();
    let agents = root.path().join(".agents/agents");
    std::fs::create_dir_all(&agents).unwrap();
    let helper = root.path().join("child_mcp_fixture.py");
    std::fs::write(
        &helper,
        r#"import json, os, sys, time
mode, log, delay = sys.argv[1:]
if mode == "fail":
    sys.exit(17)
with open(log, "a", encoding="utf-8") as events:
    events.write(f"started {os.getpid()}\n")
    events.flush()
time.sleep(int(delay) / 1000)
for line in sys.stdin:
    try:
        request = json.loads(line)
        identifier = request.get("id")
        if identifier is None:
            continue
        method = request.get("method")
        if method == "server/discover":
            result = {"resultType":"complete", "supportedVersions":["2026-07-28"], "capabilities":{"tools":{"listChanged":False}}, "ttlMs":0, "cacheScope":"private", "_meta":{"io.modelcontextprotocol/serverInfo":{"name":"agents-child-fixture", "version":"1"}}}
        elif method == "initialize":
            result = {"protocolVersion":"2026-07-28", "capabilities":{"tools":{"listChanged":False}}, "serverInfo":{"name":"agents-child-fixture", "version":"1"}}
        elif method == "tools/list":
            result = {"tools":[{"name":"ping", "description":"deterministic child fixture", "inputSchema":{"type":"object", "additionalProperties":False}}]}
        elif method == "tools/call":
            result = {"content":[{"type":"text", "text":"pong"}]}
        else:
            result = {}
        print(json.dumps({"jsonrpc":"2.0", "id":identifier, "result":result}), flush=True)
    except Exception:
        continue
with open(log, "a", encoding="utf-8") as events:
    events.write(f"stopped {os.getpid()}\n")
    events.flush()
"#,
    )
    .unwrap();
    let definition = |name: &str, mode: &str| {
        let invocation = python_invocation();
        let (command, invocation_args) = invocation.split_first().expect("non-empty argv");
        let mut args: Vec<String> = invocation_args
            .iter()
            .map(|arg| format!("\"{arg}\""))
            .collect();
        args.extend([
            format!("\"{}\"", helper.display()),
            format!("\"{mode}\""),
            format!("\"{}\"", log.display()),
            "\"450\"".to_string(),
        ]);
        format!(
            "name = \"{name}\"\ndescription = \"child lifecycle fixture\"\ninstructions = \"child fixture\"\nmodel = \"fixture-model\"\nmodel_provider = \"custom\"\nbase_url = \"{}\"\nenv_key = \"TEST_AGENT_KEY\"\nwire_api = \"responses\"\n\n[mcp_servers.fixture]\ntype = \"stdio\"\ncommand = \"{command}\"\nargs = [{}]\n",
            provider.base_url(),
            args.join(", "),
        )
    };
    std::fs::write(agents.join("alpha.toml"), definition("alpha", "serve")).unwrap();
    std::fs::write(agents.join("broken.toml"), definition("broken", "fail")).unwrap();
    root
}

async fn child_events(log: &Path, expected: usize) -> Vec<String> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let events = std::fs::read_to_string(log)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if events.len() >= expected {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "bounded child lifecycle event: {}",
            std::fs::read_to_string(log).unwrap_or_default()
        )
    })
}

fn credential_workspace(provider: &ResponsesFixture) -> TempDir {
    let root = tempfile::Builder::new()
        .prefix("mcp-agent-credentials-")
        .tempdir()
        .unwrap();
    let agents = root.path().join(".agents/agents");
    std::fs::create_dir_all(&agents).unwrap();
    for (name, model, key) in [("alpha", "model-a", "KEY_A"), ("zeta", "model-b", "KEY_B")] {
        std::fs::write(
            agents.join(format!("{name}.toml")),
            format!(
                "name=\"{name}\"\ndescription=\"{name} agent\"\ninstructions=\"Private credentials instruction\"\nmodel=\"{model}\"\nmodel_provider=\"custom\"\nbase_url=\"{}\"\nenv_key=\"{key}\"\nwire_api=\"responses\"\n",
                provider.base_url()
            ),
        )
        .unwrap();
    }
    root
}

fn redirect_workspace(base_url: &str) -> TempDir {
    let root = tempfile::tempdir().unwrap();
    let agents = root.path().join(".agents/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("redirect.toml"),
        format!(
            "name=\"redirect\"\ndescription=\"redirect fixture\"\ninstructions=\"private\"\nmodel=\"redirect-model\"\nmodel_provider=\"custom\"\nbase_url=\"{base_url}\"\nenv_key=\"TEST_AGENT_KEY\"\nwire_api=\"responses\"\n"
        ),
    )
    .unwrap();
    root
}

async fn connect(dir: &std::path::Path, with_key: bool) -> Client {
    connect_with_keys(
        dir,
        with_key.then_some(("TEST_AGENT_KEY", "fixture-secret-not-to-be-disclosed")),
        None,
    )
    .await
}

async fn connect_with_keys(
    dir: &std::path::Path,
    test_key: Option<(&str, &str)>,
    other_key: Option<(&str, &str)>,
) -> Client {
    let mut command = Command::new(BIN);
    command
        .arg("agents")
        .arg(dir)
        .env_remove("TEST_AGENT_KEY")
        .env_remove("KEY_A")
        .env_remove("KEY_B");
    if let Some((key, value)) = test_key {
        command.env(key, value);
    }
    if let Some((key, value)) = other_key {
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

async fn connect_with_progress(
    dir: &std::path::Path,
    handler: ProgressClient,
) -> rmcp::service::RunningService<RoleClient, ProgressClient> {
    let mut command = Command::new(BIN);
    command
        .arg("agents")
        .arg(dir)
        .env("TEST_AGENT_KEY", "fixture-secret-not-to-be-disclosed");
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

fn structured(result: &rmcp::model::CallToolResult) -> Value {
    result
        .structured_content
        .clone()
        .expect("structured result")
}

fn error(result: &rmcp::model::CallToolResult) -> Value {
    assert!(
        result.is_error.unwrap_or(false),
        "expected structured tool error"
    );
    serde_json::from_str(&text(result)).expect("error JSON")
}

async fn bounded<F: std::future::Future<Output = ()>>(future: F) {
    tokio::time::timeout(REQUEST_TIMEOUT, future)
        .await
        .expect("bounded test");
}

#[tokio::test]
async fn agents_cli_forms_and_invalid_selection_show_usage() {
    let fixture = ResponsesFixture::start();
    let root = workspace(&fixture);
    let subcommand = connect(root.path(), true).await;
    assert_eq!(
        subcommand
            .peer_info()
            .unwrap()
            .server_info
            .as_ref()
            .unwrap()
            .name,
        "mcp-agents"
    );

    let mut flag = Command::new(BIN);
    flag.arg("--agents")
        .arg(root.path())
        .env("TEST_AGENT_KEY", "fixture-key");
    let client: Client = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(flag).unwrap(),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("flag form starts");
    assert_eq!(
        client
            .list_tools(Default::default())
            .await
            .unwrap()
            .tools
            .len(),
        3
    );

    for args in [
        vec!["agents"],
        vec!["agents", ".", "--memory"],
        vec!["--agents", ".", "--fetch"],
    ] {
        let output = Command::new(BIN).args(args).output().await.unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Usage"));
    }
}

#[tokio::test]
async fn agents_rejects_the_2025_11_25_discover_protocol() {
    let fixture = ResponsesFixture::start();
    let root = workspace(&fixture);
    let mut command = Command::new(BIN);
    command
        .arg("agents")
        .arg(root.path())
        .env("TEST_AGENT_KEY", "fixture-key");
    let result: Result<Client, _> = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(command).unwrap(),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2025_11_25],
            },
        )
        .await;
    assert!(
        result.is_err(),
        "the server only accepts the 2026-07-28 protocol"
    );
}

#[tokio::test]
async fn agents_keep_provider_credentials_and_models_isolated() {
    bounded(async {
        let fixture = ResponsesFixture::start();
        let root = credential_workspace(&fixture);
        let token_a = "credential-a-local-fixture";
        let token_b = "credential-b-local-fixture";
        let client = connect_with_keys(
            root.path(),
            Some(("KEY_A", token_a)),
            Some(("KEY_B", token_b)),
        )
        .await;
        let catalog =
            serde_json::to_string(&client.list_tools(Default::default()).await.unwrap()).unwrap();
        assert!(!catalog.contains(token_a), "token leaked into tool catalog");
        assert!(!catalog.contains(token_b), "token leaked into tool catalog");

        let alpha = structured(
            &call_tool(
                &client,
                "spawn_agent",
                json!({"name":"alpha", "task":"delay-200 alpha"}),
            )
            .await,
        );
        let zeta = structured(
            &call_tool(
                &client,
                "spawn_agent",
                json!({"name":"zeta", "task":"delay-200 zeta"}),
            )
            .await,
        );
        let finished = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[alpha["agent_id"], zeta["agent_id"]], "timeout_ms":3000}),
            )
            .await,
        );
        assert_eq!(finished["timed_out"], false);
        let captured = fixture.requests.lock().unwrap();
        assert_eq!(captured.len(), 2);
        for request in captured.iter() {
            let rendered = request.payload.to_string();
            assert!(
                !rendered.contains(token_a),
                "token leaked into provider payload"
            );
            assert!(
                !rendered.contains(token_b),
                "token leaked into provider payload"
            );
            match request.payload["model"].as_str().unwrap() {
                "model-a" => assert!(
                    request.authorization.as_deref() == Some("Bearer credential-a-local-fixture"),
                    "model A used the wrong credential"
                ),
                "model-b" => assert!(
                    request.authorization.as_deref() == Some("Bearer credential-b-local-fixture"),
                    "model B used the wrong credential"
                ),
                _ => panic!("unexpected fixture model"),
            }
        }
        let rendered_result = finished.to_string();
        assert!(
            !rendered_result.contains(token_a),
            "token leaked into tool output"
        );
        assert!(
            !rendered_result.contains(token_b),
            "token leaked into tool output"
        );
    })
    .await;
}

#[tokio::test]
async fn agents_capacity_is_eight_and_running_work_is_cleaned_up() {
    bounded(async {
        let fixture = ResponsesFixture::start();
        let root = workspace(&fixture);
        let client = connect(root.path(), true).await;
        let retained = structured(
            &call_tool(
                &client,
                "spawn_agent",
                json!({"name":"alpha", "task":"retained result"}),
            )
            .await,
        );
        let retained_id = retained["agent_id"].as_str().unwrap().to_owned();
        let retained_result = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[retained_id], "timeout_ms":3000}),
            )
            .await,
        )["agents"][0]["result"]
            .clone();
        let mut ids = Vec::new();
        for _ in 0..8 {
            let spawned = structured(
                &call_tool(
                    &client,
                    "spawn_agent",
                    json!({"name":"alpha", "task":"delay-1000 capacity"}),
                )
                .await,
            );
            assert_eq!(spawned["status"], "running");
            ids.push(spawned["agent_id"].as_str().unwrap().to_owned());
        }
        let ninth = error(
            &call_tool(
                &client,
                "spawn_agent",
                json!({"name":"alpha", "task":"delay-1000 ninth"}),
            )
            .await,
        );
        assert_eq!(ninth["kind"], "capacity_exceeded");
        let resume_at_capacity = error(
            &call_tool(
                &client,
                "send_input",
                json!({"target":retained_id, "message":"must not start", "interrupt":false}),
            )
            .await,
        );
        assert_eq!(resume_at_capacity["kind"], "capacity_exceeded");
        let still_completed = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[retained_id], "timeout_ms":0}),
            )
            .await,
        );
        assert_eq!(still_completed["agents"][0]["status"], "completed");
        assert_eq!(still_completed["agents"][0]["result"], retained_result);
        let complete = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":ids, "timeout_ms":3000}),
            )
            .await,
        );
        assert_eq!(complete["timed_out"], false);
        assert!(
            complete["agents"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| a["status"] == "completed")
        );
        let resumed = structured(
            &call_tool(
                &client,
                "send_input",
                json!({"target":retained_id, "message":"permit released", "interrupt":false}),
            )
            .await,
        );
        assert_eq!(resumed["status"], "running");
        let resumed_done = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[retained_id], "timeout_ms":3000}),
            )
            .await,
        );
        assert_eq!(resumed_done["agents"][0]["status"], "completed");
    })
    .await;
}

#[tokio::test]
async fn agents_do_not_follow_cross_origin_provider_redirects() {
    bounded(async {
        let fixture = RedirectFixture::start();
        let root = redirect_workspace(&fixture.base_url);
        let client = connect(root.path(), true).await;
        let spawned = structured(
            &call_tool(
                &client,
                "spawn_agent",
                json!({"name":"redirect", "task":"redirect task"}),
            )
            .await,
        );
        assert_eq!(spawned["status"], "running");
        let waited = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[spawned["agent_id"]], "timeout_ms":3000}),
            )
            .await,
        );
        assert_eq!(waited["agents"][0]["status"], "failed");
        assert_eq!(waited["agents"][0]["error"]["kind"], "provider_error");
        assert_eq!(
            waited["agents"][0]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["agent_id", "error", "name", "status", "total_elapsed_ms"]
        );
        assert_eq!(*fixture.redirected_requests.lock().unwrap(), 0);
    })
    .await;
}

#[tokio::test]
async fn agents_context_limit_is_safe_transactional_and_does_not_compact() {
    bounded(async {
        let fixture = ResponsesFixture::start();
        let root = workspace(&fixture);
        let client = connect(root.path(), true).await;
        let spawned = structured(
            &call_tool(
                &client,
                "spawn_agent",
                json!({"name":"alpha", "task":"baseline conversation"}),
            )
            .await,
        );
        let id = spawned["agent_id"].as_str().unwrap().to_owned();
        let initial = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[id], "timeout_ms":3000}),
            )
            .await,
        );
        assert_eq!(initial["agents"][0]["status"], "completed");

        structured(
            &call_tool(
                &client,
                "send_input",
                json!({"target":id, "message":"context-limit private conversation", "interrupt":false}),
            )
            .await,
        );
        let failed = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[id], "timeout_ms":3000}),
            )
            .await,
        );
        assert_eq!(failed["agents"][0]["status"], "failed");
        assert_eq!(failed["agents"][0]["error"]["kind"], "context_limit");
        assert!(!failed.to_string().contains("private conversation"));
        assert!(!failed.to_string().contains("secret provider payload"));
        assert_eq!(fixture.requests.lock().unwrap().len(), 2);

        let repeated = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[id], "timeout_ms":0}),
            )
            .await,
        );
        assert_eq!(repeated["agents"][0]["error"]["kind"], "context_limit");
        structured(
            &call_tool(
                &client,
                "send_input",
                json!({"target":id, "message":"recover narrowly", "interrupt":false}),
            )
            .await,
        );
        let recovered = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[id], "timeout_ms":3000}),
            )
            .await,
        );
        assert_eq!(recovered["agents"][0]["status"], "completed");
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 3, "no compaction, summarization, or retry request");
        let replayed = requests[2].payload["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(user_text)
            .collect::<Vec<_>>();
        assert_eq!(replayed, ["baseline conversation", "recover narrowly"]);
        assert!(
            requests[2].payload["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["role"] == "assistant")
        );
    })
    .await;
}

#[tokio::test]
async fn agents_discover_schema_errors_and_lifecycle_are_real_protocol_e2e() {
    bounded(async {
        let fixture = ResponsesFixture::start();
        let root = workspace(&fixture);

        // Credentials are injected only in this child process, never through
        // process-global environment mutation in the test runner.
        let missing = connect(root.path(), false).await;
        let missing_error = error(
            &call_tool(
                &missing,
                "spawn_agent",
                json!({"name":"alpha", "task":"no key"}),
            )
            .await,
        );
        assert_eq!(missing_error["kind"], "missing_environment_variable");
        assert!(
            missing_error["message"]
                .as_str()
                .unwrap()
                .contains("TEST_AGENT_KEY")
        );

        let malformed = error(
            &call_tool(
                &missing,
                "spawn_agent",
                json!({"name":"alpha", "task":"no key", "unexpected":true}),
            )
            .await,
        );
        assert_eq!(malformed["kind"], "invalid_request");
        assert!(
            !missing_error
                .to_string()
                .contains("fixture-secret-not-to-be-disclosed")
        );

        let client = connect(root.path(), true).await;
        let info = client.peer_info().unwrap();
        assert_eq!(
            info.protocol_version,
            rmcp::model::ProtocolVersion::V_2026_07_28
        );
        let implementation = info.server_info.as_ref().unwrap();
        assert_eq!(implementation.name, "mcp-agents");
        assert_eq!(implementation.version, env!("CARGO_PKG_VERSION"));

        let listed = client.list_tools(Default::default()).await.unwrap();
        assert_eq!(listed.ttl_ms, Some(0));
        assert_eq!(listed.cache_scope, Some(rmcp::model::CacheScope::Private));
        let tools = listed.tools;
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.name.as_ref())
                .collect::<Vec<_>>(),
            ["spawn_agent", "send_input", "wait_agent"]
        );
        let spawn = serde_json::to_value(&tools[0]).unwrap();
        assert_eq!(spawn["inputSchema"]["additionalProperties"], false);
        assert_eq!(
            spawn["inputSchema"]["properties"]["name"]["enum"],
            json!(["alpha", "zeta"])
        );
        assert_eq!(spawn["inputSchema"]["properties"]["task"]["minLength"], 1);
        assert_eq!(spawn["annotations"]["destructiveHint"], true);
        assert_eq!(spawn["annotations"]["openWorldHint"], true);
        assert!(
            spawn["description"]
                .as_str()
                .unwrap()
                .contains("- alpha: First fixture agent\n- zeta: Second fixture agent")
        );
        assert!(!spawn.to_string().contains("Private instruction"));
        assert!(!spawn.to_string().contains("TEST_AGENT_KEY"));
        let input = serde_json::to_value(&tools[1]).unwrap();
        assert_eq!(input["inputSchema"]["additionalProperties"], false);
        assert_eq!(
            input["inputSchema"]["properties"]["interrupt"]["default"],
            false
        );
        let wait = serde_json::to_value(&tools[2]).unwrap();
        assert_eq!(wait["inputSchema"]["properties"]["targets"]["minItems"], 1);
        assert_eq!(
            wait["inputSchema"]["properties"]["targets"]["uniqueItems"],
            true
        );
        assert_eq!(
            wait["inputSchema"]["properties"]["timeout_ms"]["minimum"],
            0
        );
        assert_eq!(
            wait["inputSchema"]["properties"]["timeout_ms"]["maximum"],
            300000
        );
        assert_eq!(wait["annotations"]["readOnlyHint"], true);
        assert_eq!(wait["annotations"]["openWorldHint"], false);

        let delayed_start = Instant::now();
        let first = structured(
            &call_tool(
                &client,
                "spawn_agent",
                json!({"name":"alpha", "task":"delay-200 first"}),
            )
            .await,
        );
        assert!(delayed_start.elapsed() < Duration::from_millis(150));
        assert_eq!(first["status"], "running");
        assert!(first.get("result").is_none());
        assert_eq!(
            first
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["agent_id", "name", "status"]
        );
        let second = structured(
            &call_tool(
                &client,
                "spawn_agent",
                json!({"name":"zeta", "task":"delay-200 second"}),
            )
            .await,
        );
        let first_id = first["agent_id"].as_str().unwrap().to_owned();
        let second_id = second["agent_id"].as_str().unwrap().to_owned();
        assert_ne!(first_id, second_id);

        let immediate = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[first_id, second_id], "timeout_ms":0}),
            )
            .await,
        );
        assert_eq!(immediate["timed_out"], true);
        assert!(
            immediate["agents"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| a["status"] == "running")
        );
        for agent in immediate["agents"].as_array().unwrap() {
            assert_eq!(
                agent
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                ["activity", "agent_id", "name", "status", "total_elapsed_ms"]
            );
            assert!(matches!(
                agent["activity"]["phase"].as_str(),
                Some("starting" | "model")
            ));
            assert!(agent["activity"]["summary"].as_str().unwrap().len() <= 120);
            assert!(agent["total_elapsed_ms"].as_u64().is_some());
            assert!(agent["activity"]["activity_elapsed_ms"].as_u64().is_some());
        }

        let started = Instant::now();
        let completed = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[first_id, second_id], "timeout_ms":3000}),
            )
            .await,
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(completed["timed_out"], false);
        for agent in completed["agents"].as_array().unwrap() {
            assert_eq!(agent["status"], "completed");
            assert_eq!(
                agent
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                ["agent_id", "name", "result", "status", "total_elapsed_ms"]
            );
        }
        let repeated_start = Instant::now();
        let repeated = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[first_id], "timeout_ms":3000}),
            )
            .await,
        );
        assert!(repeated_start.elapsed() < Duration::from_millis(100));
        assert_eq!(
            repeated["agents"][0]["result"],
            "provider result: delay-200 first"
        );
        assert_eq!(
            completed["agents"]
                .as_array()
                .unwrap()
                .iter()
                .find(|agent| agent["agent_id"] == first_id)
                .unwrap()["total_elapsed_ms"],
            repeated["agents"][0]["total_elapsed_ms"],
            "terminal duration is immutable across later snapshots"
        );

        let ack = structured(
            &call_tool(
                &client,
                "send_input",
                json!({"target":first_id, "message":"follow-up", "interrupt":false}),
            )
            .await,
        );
        assert_eq!(ack["status"], "running");
        assert!(ack.get("result").is_none());
        assert_eq!(
            ack.as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["accepted", "agent_id", "status"]
        );
        let follow_up = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[first_id], "timeout_ms":3000}),
            )
            .await,
        );
        assert_eq!(
            follow_up["agents"][0]["result"],
            "provider result: follow-up"
        );

        let late = structured(
            &call_tool(
                &client,
                "spawn_agent",
                json!({"name":"alpha", "task":"delay-200 timeout"}),
            )
            .await,
        );
        let late_id = late["agent_id"].as_str().unwrap();
        let timeout = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[late_id], "timeout_ms":20}),
            )
            .await,
        );
        assert_eq!(timeout["timed_out"], true);
        assert_eq!(timeout["agents"][0]["status"], "running");
        let eventual = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[late_id], "timeout_ms":3000}),
            )
            .await,
        );
        assert_eq!(eventual["timed_out"], false);
        assert_eq!(
            eventual["agents"][0]["result"],
            "provider result: delay-200 timeout"
        );

        let unknown = error(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":["missing"], "timeout_ms":0}),
            )
            .await,
        );
        assert_eq!(unknown["kind"], "unknown_agent");
        let duplicate = error(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[first_id, first_id], "timeout_ms":0}),
            )
            .await,
        );
        assert_eq!(duplicate["kind"], "invalid_request");

        let log = fixture.requests.lock().unwrap();
        let replay = log
            .iter()
            .find(|request| {
                request.payload["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| user_text(item) == Some("follow-up"))
            })
            .expect("follow-up is replayed to the provider");
        let replayed_text = replay.payload["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(user_text)
            .collect::<Vec<_>>();
        assert_eq!(replayed_text, ["delay-200 first", "follow-up"]);
        assert_eq!(
            log.iter()
                .filter(|request| {
                    request.payload["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|item| user_text(item) == Some("delay-200 timeout"))
                })
                .count(),
            1,
            "a wait timeout never starts another provider run"
        );
    })
    .await;
}

#[tokio::test]
async fn agents_wait_reports_standard_mcp_progress_until_response() {
    bounded(async {
        let fixture = ResponsesFixture::start();
        let root = workspace(&fixture);
        let handler = ProgressClient::default();
        let notifications = handler.notifications.clone();
        let client = connect_with_progress(root.path(), handler).await;
        let spawned_response = client
            .call_tool(
                CallToolRequestParams::new("spawn_agent").with_arguments(
                    json!({"name":"alpha", "task":"delay-200 progress"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .expect("spawn response");
        let spawned = structured(&spawned_response);
        let token = "wait-activity-token";
        let mut request = CallToolRequestParams::new("wait_agent").with_arguments(
            json!({"targets":[spawned["agent_id"]], "timeout_ms":3000})
                .as_object()
                .unwrap()
                .clone(),
        );
        request.meta = Some(serde_json::from_value(json!({"progressToken": token})).unwrap());
        let response = client.call_tool(request).await.expect("wait response");
        assert_eq!(structured(&response)["timed_out"], false);

        let received = notifications.lock().unwrap().clone();
        assert!(
            received.len() >= 2,
            "initial and terminal activity updates arrive"
        );
        let request_token = received[0].progress_token.clone();
        for notification in &received {
            assert_eq!(notification.progress_token, request_token);
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
        let count = received.len();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(notifications.lock().unwrap().len(), count);
    })
    .await;
}

#[tokio::test]
async fn agents_child_mcp_lifecycle_is_async_isolated_and_cleaned_up() {
    bounded(async {
        let fixture = ResponsesFixture::start();
        let log_dir = tempfile::tempdir().unwrap();
        let log = log_dir.path().join("child-events.log");
        let root = child_mcp_workspace(&fixture, &log);
        let client = connect(root.path(), true).await;

        // Child startup takes 450ms, while spawn must only schedule the run.
        let started = Instant::now();
        let first = structured(
            &call_tool(
                &client,
                "spawn_agent",
                json!({"name":"alpha", "task":"first child-backed run"}),
            )
            .await,
        );
        assert!(started.elapsed() < Duration::from_millis(250));
        assert_eq!(first["status"], "running");
        let first_id = first["agent_id"].as_str().unwrap().to_owned();
        assert_eq!(child_events(&log, 1).await.len(), 1);
        let first_done = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[first_id], "timeout_ms":3000}),
            )
            .await,
        );
        assert_eq!(
            first_done["agents"][0]["status"], "completed",
            "{first_done}"
        );
        let events = child_events(&log, 2).await;
        assert!(events[0].starts_with("started "));
        assert!(events[1].starts_with("stopped "));

        // A terminal session resumes with a new process/connection, while its
        // provider request still contains the complete prior conversation.
        let ack = structured(
            &call_tool(
                &client,
                "send_input",
                json!({"target":first_id, "message":"second child-backed run", "interrupt":false}),
            )
            .await,
        );
        assert_eq!(ack["status"], "running");
        let second_done = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[first_id], "timeout_ms":3000}),
            )
            .await,
        );
        assert_eq!(second_done["agents"][0]["status"], "completed");
        let events = child_events(&log, 4).await;
        assert!(events[2].starts_with("started "));
        assert!(events[3].starts_with("stopped "));
        assert_ne!(events[0], events[2], "run #2 reused run #1 child process");

        {
            let requests = fixture.requests.lock().unwrap();
            let replay = requests
                .iter()
                .find(|request| {
                    request.payload["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|item| user_text(item) == Some("second child-backed run"))
                })
                .expect("resumed provider request");
            assert_eq!(
                replay.payload["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(user_text)
                    .collect::<Vec<_>>(),
                ["first child-backed run", "second child-backed run"]
            );
            assert!(
                replay.payload["tools"]
                    .to_string()
                    .contains("fixture__ping"),
                "the fresh child connection supplied its tool catalog"
            );
        }

        // Concurrent terminal resumes serialize into one active run. The
        // slower first request gives the second caller time to queue safely.
        let (resume_a, resume_b) = tokio::join!(
            call_tool(
                &client,
                "send_input",
                json!({"target":first_id, "message":"delay-200 concurrent-a", "interrupt":false}),
            ),
            call_tool(
                &client,
                "send_input",
                json!({"target":first_id, "message":"delay-200 concurrent-b", "interrupt":false}),
            )
        );
        assert_eq!(structured(&resume_a)["accepted"], true);
        assert_eq!(structured(&resume_b)["accepted"], true);
        let concurrent_done = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[first_id], "timeout_ms":3000}),
            )
            .await,
        );
        assert_eq!(concurrent_done["agents"][0]["status"], "completed");
        let events = child_events(&log, 6).await;
        assert!(events[4].starts_with("started "));
        assert!(events[5].starts_with("stopped "));

        // A startup failure is asynchronous too, preserves the spawned id,
        // and is surfaced as the hardened public error kind.
        let failed = structured(
            &call_tool(
                &client,
                "spawn_agent",
                json!({"name":"broken", "task":"broken child startup"}),
            )
            .await,
        );
        let failed_id = failed["agent_id"].as_str().unwrap().to_owned();
        let failed_wait = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[failed_id], "timeout_ms":3000}),
            )
            .await,
        );
        assert_eq!(failed_wait["agents"][0]["agent_id"], failed_id);
        assert_eq!(failed_wait["agents"][0]["status"], "failed");
        assert_eq!(
            failed_wait["agents"][0]["error"]["kind"],
            "child_mcp_startup_error"
        );

        // Interrupt while the next child is still in its deliberate startup
        // delay. The queued input continues through a fresh child connection.
        let interrupted = structured(
            &call_tool(
                &client,
                "spawn_agent",
                json!({"name":"alpha", "task":"interrupt child startup"}),
            )
            .await,
        );
        let interrupted_id = interrupted["agent_id"].as_str().unwrap().to_owned();
        child_events(&log, 7).await;
        let interruption_ack = structured(
            &call_tool(
                &client,
                "send_input",
                json!({"target":interrupted_id, "message":"cancel", "interrupt":true}),
            )
            .await,
        );
        assert_eq!(interruption_ack["status"], "running");
        let interrupted_wait = structured(
            &call_tool(
                &client,
                "wait_agent",
                json!({"targets":[interrupted_id], "timeout_ms":3000}),
            )
            .await,
        );
        assert_eq!(interrupted_wait["agents"][0]["status"], "completed");
        assert_eq!(
            interrupted_wait["agents"][0]["result"],
            "provider result: cancel"
        );
        let events = child_events(&log, 9).await;
        assert!(events[7].starts_with("started "));
        assert!(events[8].starts_with("stopped "));
        assert_ne!(events[6], events[7], "interruption reused its child");
        #[cfg(unix)]
        {
            let interrupted_pid = events[6].split_once(' ').unwrap().1;
            assert!(
                !std::process::Command::new("kill")
                    .args(["-0", interrupted_pid])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .unwrap()
                    .success(),
                "cancelled startup child is still alive"
            );
        }
    })
    .await;
}
