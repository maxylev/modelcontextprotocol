//! Deterministic fixtures for the skills + agents acceptance suites.
//!
//! - [`Workspace`] builds the canonical acceptance workspace: two skills
//!   (one with a supporting resource), a duplicate/precedence fixture, a
//!   malformed skill, two OpenRouter-configured agents (`reviewer`,
//!   `researcher`) wired to the real filesystem MCP server as a child, and
//!   the deterministic `src/auth.rs` / `src/retry.rs` / contract fixtures.
//! - [`LocalProvider`] is a deterministic `tiny_http` Responses endpoint used
//!   by the offline agent lifecycle tests (no network, no real model).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use serde_json::{Value, json};
use tiny_http::{Header, Response, Server, StatusCode};

mod inner_names {
    // Hidden markers that exist *only* inside the skill resource. They must
    // never appear in prompts, agent definitions, SKILL.md bodies, or any
    // other LLM-visible fixture content.
    pub const RELEASE_CHANNEL: &str = "nebula-7";
    pub const RELEASE_MARKER: &str = "SKILL-RESOURCE-OK";
}
pub use inner_names::{RELEASE_CHANNEL, RELEASE_MARKER};

pub const SKILL_DIR: &str = ".agents/skills/release-audit";
pub const RESOURCE_RELATIVE: &str = "references/release-contract.md";
pub const AUTH_PATH: &str = "src/auth.rs";
pub const RETRY_PATH: &str = "src/retry.rs";
pub const RETRY_CONTRACT_PATH: &str = "tests/retry-contract.txt";

pub const REVIEWER_NAME: &str = "reviewer";
pub const RESEARCHER_NAME: &str = "researcher";

/// Canonical acceptance workspace.
pub struct Workspace {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
}

impl Workspace {
    /// Build the full acceptance fixture. `bin` is the
    /// `CARGO_BIN_EXE_modelcontextprotocol` path inserted into the agent
    /// child-MCP configuration.
    pub fn new(bin: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("mcp-acceptance-workspace-")
            .tempdir()
            .expect("create fixture workspace");
        let root = dir.path().to_path_buf();
        write_workspace(&root, bin);
        Self { _dir: dir, root }
    }

    pub fn skill_dir(&self) -> PathBuf {
        self.root.join(SKILL_DIR)
    }
    pub fn resource_path(&self) -> PathBuf {
        self.root.join(SKILL_DIR).join(RESOURCE_RELATIVE)
    }
    pub fn auth(&self) -> PathBuf {
        self.root.join(AUTH_PATH)
    }
    pub fn retry(&self) -> PathBuf {
        self.root.join(RETRY_PATH)
    }
    pub fn retry_contract(&self) -> PathBuf {
        self.root.join(RETRY_CONTRACT_PATH)
    }
}

fn write_workspace(root: &Path, bin: &str) {
    let rel = |path: &str| root.join(path);

    // --- skills -----------------------------------------------------------
    let release = rel(".agents/skills/release-audit");
    std::fs::create_dir_all(release.join("references")).expect("create release skill dir");
    std::fs::write(
        release.join("SKILL.md"),
        r#"---
name: release-audit
description: Use this skill for release-readiness, release validation, or pre-deployment engineering audits.
---

For a release-readiness report:

1. Read `references/release-contract.md` before producing the final report.
2. Treat the release contract as authoritative for release metadata.
3. Delegate independent implementation checks when subagents are available.
4. Report concrete findings and cite the relevant workspace paths.
5. Do not invent release metadata.
"#,
    )
    .expect("write release-audit SKILL.md");

    // The two hidden markers exist only here, nowhere else in the workspace.
    std::fs::write(
        release.join(RESOURCE_RELATIVE),
        format!(
            "# Release Contract\n\nRelease channel: {}\n\nRequired report marker: {}\n\nThe release report must include both the release channel and the required report marker.\n",
            RELEASE_CHANNEL, RELEASE_MARKER
        ),
    )
    .expect("write release contract");

    let reviewer_guidance = rel(".agents/skills/reviewer-guidance");
    std::fs::create_dir_all(&reviewer_guidance).expect("create reviewer-guidance dir");
    std::fs::write(
        reviewer_guidance.join("SKILL.md"),
        r#"---
name: reviewer-guidance
description: Focus code reviews on correctness, authorization boundaries, regressions, and concrete evidence.
---

Review the requested code carefully.

Prefer concrete findings with file paths and explain why the behavior is incorrect.
"#,
    )
    .expect("write reviewer-guidance SKILL.md");

    // Duplicate declared name in a lower-precedence root: the canonical
    // `.agents` copy must win and the catalog must contain the name once.
    let claude_dup = root.join(".claude/skills/release-audit");
    std::fs::create_dir_all(&claude_dup).expect("create claude dup dir");
    std::fs::write(
        claude_dup.join("SKILL.md"),
        r#"---
name: release-audit
description: DUPLICATE lower-precedence release-audit that must not win.
---

This duplicate must be ignored: the canonical .agents copy wins.
"#,
    )
    .expect("write claude duplicate");

    // Malformed skill that must be skipped without breaking the server.
    let broken = root.join(".claude/skills/broken");
    std::fs::create_dir_all(&broken).expect("create broken dir");
    std::fs::write(broken.join("SKILL.md"), "this is not YAML frontmatter\n")
        .expect("write malformed skill");

    // --- deterministic application fixtures --------------------------------
    let src = rel("src");
    std::fs::create_dir_all(&src).expect("create src");
    std::fs::write(
        src.join("auth.rs"),
        "pub fn is_authorized(token: &str) -> bool {\n    token.is_empty() || token == \"valid-token\"\n}\n",
    )
    .expect("write auth.rs");
    std::fs::write(src.join("retry.rs"), "pub const MAX_RETRIES: usize = 5;\n")
        .expect("write retry.rs");
    let tests = rel("tests");
    std::fs::create_dir_all(&tests).expect("create tests dir");
    std::fs::write(
        tests.join("retry-contract.txt"),
        "Production retry contract: MAX_RETRIES must be 3.\n",
    )
    .expect("write retry contract");
    std::fs::write(rel("README.md"), "# fixture workspace\n").expect("write README");

    // --- agents ------------------------------------------------------------
    std::fs::create_dir_all(rel(".agents/agents")).expect("create agents dir");
    write_agent(
        &rel(".agents/agents/reviewer.toml"),
        REVIEWER_NAME,
        "Reviews implementation code for correctness, authorization bugs, regressions, and concrete evidence.",
        r#"You are a focused code reviewer.

Inspect only the files relevant to the delegated task.
Use available workspace tools for evidence.
Return concise findings with exact paths and explain impact.
Do not speculate when the workspace provides direct evidence."#,
        &["reviewer-guidance"],
        bin,
        root,
    );
    write_agent(
        &rel(".agents/agents/researcher.toml"),
        RESEARCHER_NAME,
        "Investigates implementation and repository contracts, comparing code with documented expected behavior.",
        r#"You are a focused repository researcher.

Use available workspace tools to inspect the implementation and its local contracts.
Compare concrete values and return concise evidence with paths."#,
        &[],
        bin,
        root,
    );
}

/// Serialize a canonical agent definition (TOML) with an OpenRouter custom
/// provider and a stdio child filesystem MCP server rooted at the workspace.
/// The provider credential is referenced by env key only; no secret is ever
/// written into the fixture.
fn write_agent(
    path: &Path,
    name: &str,
    description: &str,
    instructions: &str,
    skills: &[&str],
    bin: &str,
    workspace: &Path,
) {
    #[derive(serde::Serialize)]
    struct McpServer {
        #[serde(rename = "type")]
        kind: &'static str,
        command: String,
        args: Vec<String>,
    }
    #[derive(serde::Serialize)]
    struct Agent {
        name: String,
        description: String,
        developer_instructions: String,
        model: &'static str,
        model_provider: &'static str,
        base_url: &'static str,
        env_key: &'static str,
        wire_api: &'static str,
        model_reasoning_effort: &'static str,
        max_turns: u32,
        sandbox_mode: &'static str,
        skills: Vec<String>,
        mcp_servers: BTreeMap<String, McpServer>,
    }
    let agent = Agent {
        name: name.to_string(),
        description: description.to_string(),
        developer_instructions: instructions.to_string(),
        model: "openai/gpt-5.6-luna",
        model_provider: "custom",
        base_url: "https://openrouter.ai/api/v1",
        env_key: "OPENROUTER_API_KEY",
        wire_api: "responses",
        model_reasoning_effort: "medium",
        max_turns: 12,
        sandbox_mode: "read-only",
        skills: skills.iter().map(|s| s.to_string()).collect(),
        mcp_servers: BTreeMap::from([(
            "workspace".to_string(),
            McpServer {
                kind: "stdio",
                command: bin.to_string(),
                args: vec!["filesystem".to_string(), workspace.display().to_string()],
            },
        )]),
    };
    let toml = toml::to_string(&agent).expect("serialize agent TOML");
    std::fs::write(path, toml).expect("write agent TOML");
}

// ---------------------------------------------------------------------------
// Deterministic local Responses provider
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct CapturedRequest {
    pub url: String,
    pub authorization: Option<String>,
    pub payload: Value,
}

/// A real loopback HTTP Responses endpoint for offline agent lifecycle
/// tests. Each accepted request is handled independently so concurrent
/// provider calls genuinely overlap.
pub struct LocalProvider {
    pub port: u16,
    pub requests: Arc<Mutex<Vec<CapturedRequest>>>,
    alive: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl LocalProvider {
    pub fn start() -> Self {
        let server = Server::http("127.0.0.1:0").expect("start local provider");
        let port = server.server_addr().to_ip().expect("IP listener").port();
        let requests: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let captured = requests.clone();
        let running = alive.clone();
        let worker = std::thread::spawn(move || {
            while running.load(Ordering::Acquire) {
                let Ok(Some(mut request)) = server.recv_timeout(Duration::from_millis(25)) else {
                    continue;
                };
                let captured = captured.clone();
                std::thread::spawn(move || {
                    let url = request.url().to_owned();
                    let mut body = String::new();
                    request
                        .as_reader()
                        .read_to_string(&mut body)
                        .expect("read provider request");
                    let payload: Value = serde_json::from_str(&body).unwrap_or_default();
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
                        url,
                        authorization,
                        payload: payload.clone(),
                    });
                    let latest = payload["input"]
                        .as_array()
                        .and_then(|input| {
                            input.iter().rev().find_map(|item| {
                                (item["role"].as_str() == Some("user")).then(|| {
                                    item["content"]
                                        .as_array()
                                        .map(|blocks| {
                                            blocks
                                                .iter()
                                                .find_map(|b| b["text"].as_str())
                                                .unwrap_or_default()
                                        })
                                        .unwrap_or_default()
                                })
                            })
                        })
                        .unwrap_or_default();
                    if latest.contains("delay-")
                        && let Some(ms) = latest
                            .split("delay-")
                            .nth(1)
                            .and_then(|s| s.split_whitespace().next())
                            .and_then(|s| s.parse::<u64>().ok())
                    {
                        std::thread::sleep(Duration::from_millis(ms));
                    }
                    if latest.contains("context-limit") {
                        let response = Response::from_string(
                            json!({"error":{"code":"context_length_exceeded","message":"secret provider payload"}})
                                .to_string(),
                        )
                        .with_status_code(StatusCode(400))
                        .with_header(Header::from_bytes("content-type", "application/json").unwrap());
                        let _ = request.respond(response);
                        return;
                    }
                    let answer = format!("provider result: {latest}");
                    let response = json!({
                        "output": [{"type":"message", "role":"assistant", "content":[{"type":"output_text","text":answer}]}],
                        "output_text": answer,
                    });
                    let response = Response::from_string(response.to_string())
                        .with_status_code(StatusCode(200))
                        .with_header(
                            Header::from_bytes("content-type", "application/json").unwrap(),
                        );
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

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    pub fn assert_no_compact_traffic(&self) {
        let hits = self
            .requests
            .lock()
            .expect("request log")
            .iter()
            .filter(|request| request.url.contains("/compact"))
            .count();
        assert_eq!(hits, 0, "unexpected Responses compaction traffic");
    }
}

impl Drop for LocalProvider {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Local-provider agent workspace used by the offline agent lifecycle tests.
pub struct LocalAgentWorkspace {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
}

impl LocalAgentWorkspace {
    /// Create one agent with `name`/`description`/`instructions` pointed at
    /// `provider_base_url`, with `max_turns`, optional `skills`, and an
    /// optional stdio child MCP server.
    pub fn single(
        name: &str,
        description: &str,
        instructions: &str,
        provider_base_url: &str,
        max_turns: u32,
        skills: &[&str],
        child: Option<(String, Vec<String>)>,
    ) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("mcp-agent-local-")
            .tempdir()
            .expect("create local agent workspace");
        let root = dir.path().to_path_buf();
        let agents = root.join(".agents/agents");
        std::fs::create_dir_all(&agents).expect("create agents dir");

        if !skills.is_empty() {
            // Reuse the shared reviewer-guidance skill so skill-preload
            // behavior mirrors the reviewer acceptance fixture.
            let guidance = root.join(".agents/skills/reviewer-guidance");
            std::fs::create_dir_all(&guidance).expect("create skill dir");
            std::fs::write(
                guidance.join("SKILL.md"),
                r#"---
name: reviewer-guidance
description: Focus code reviews on correctness, authorization boundaries, regressions, and concrete evidence.
---

Review the requested code carefully.

Prefer concrete findings with file paths and explain why the behavior is incorrect.
"#,
            )
            .expect("write skill");
        }

        let mut servers = BTreeMap::new();
        if let Some((command, args)) = child {
            servers.insert(
                "child".to_string(),
                json!({"type":"stdio","command":command,"args":args}),
            );
        }
        let definition = json!({
            "name": name,
            "description": description,
            "developer_instructions": instructions,
            "model": "fixture-model",
            "model_provider": "custom",
            "base_url": provider_base_url,
            "env_key": "TEST_AGENT_KEY",
            "wire_api": "responses",
            "max_turns": max_turns,
            "skills": skills,
            "mcp_servers": servers,
        });
        let toml = toml::to_string(&definition).expect("serialize local agent TOML");
        std::fs::write(agents.join(format!("{name}.toml")), toml).expect("write agent TOML");
        Self { _dir: dir, root }
    }
}
