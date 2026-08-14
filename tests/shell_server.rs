//! End-to-end tests for the shell server: spawns the real binary over stdio,
//! drives it with the rmcp client, and executes commands via the compiled
//! test binary itself (re-invoked in a helper mode through libtest's
//! `--exact` filter), so no platform shell utilities are required.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{Client, call_tool, text};
use rmcp::{
    service::{ClientLifecycleMode, ClientServiceExt},
    transport::TokioChildProcess,
};
use tempfile::TempDir;
use tokio::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_modelcontextprotocol");
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

fn structured(result: &rmcp::model::CallToolResult) -> serde_json::Value {
    result
        .structured_content
        .clone()
        .expect("result has structured content")
}

fn reported_cwd(output: &str) -> PathBuf {
    output
        .lines()
        .find_map(|line| std::fs::canonicalize(line.trim()).ok())
        .expect("helper output contains an existing cwd")
}

/// Marker argument that switches the test binary into helper mode.
const HELPER_MARKER: &str = "test_helper_command";

/// The test binary doubles as a deterministic command helper: when spawned
/// with `--exact test_helper_command <mode> ...`, libtest runs only this
/// test, which performs the requested action and exits.
#[test]
fn test_helper_command() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 || args.get(3).map(String::as_str) != Some(HELPER_MARKER) {
        return;
    }
    use std::io::Write;
    match args[4].as_str() {
        // Prints every argv entry on its own line.
        "echo" => {
            for value in &args[5..] {
                println!("{value}");
            }
        }
        "stderr" => eprintln!("helper stderr line"),
        "exit" => std::process::exit(args[5].parse().expect("exit code")),
        "sleep" => std::thread::sleep(Duration::from_millis(args[5].parse().expect("sleep ms"))),
        "cwd" => println!("{}", std::env::current_dir().expect("cwd").display()),
        "big" => std::io::stdout()
            .write_all(&vec![b'x'; 1_200_000])
            .expect("write big stdout"),
        "bigstderr" => std::io::stderr()
            .write_all(&vec![b'y'; 1_200_000])
            .expect("write big stderr"),
        _ => {}
    }
}

/// Invocation (program, argv) that runs the helper in the given mode.
fn helper(mode: &str, payload: &[&str]) -> (String, Vec<String>) {
    let exe = std::env::current_exe().expect("test executable path");
    let mut args = vec![
        "--nocapture".to_string(),
        "--exact".to_string(),
        HELPER_MARKER.to_string(),
        mode.to_string(),
    ];
    args.extend(payload.iter().map(|p| p.to_string()));
    (exe.to_string_lossy().into_owned(), args)
}

async fn connect_shell(dirs: &[&Path]) -> Client {
    let mut cmd = Command::new(BIN);
    cmd.arg("shell");
    for dir in dirs {
        cmd.arg(dir);
    }
    let client: Client = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(cmd).expect("spawn shell server"),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("shell server starts");
    client
}

async fn run_test<F>(future: F) -> F::Output
where
    F: std::future::Future<Output = ()>,
{
    tokio::time::timeout(REQUEST_TIMEOUT, future)
        .await
        .expect("test completed within timeout")
}

fn tmpdir() -> TempDir {
    tempfile::Builder::new()
        .prefix("mcp-shell-test-")
        .tempdir()
        .expect("create temp dir")
}

fn join(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}

// ---------------------------------------------------------------------------
// Startup / CLI forms
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shell_starts_through_subcommand() {
    let dir = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    run_test(async move {
        let tools = client
            .list_tools(Default::default())
            .await
            .expect("list tools");
        assert!(tools.tools.iter().any(|t| t.name == "execute_command"));
    })
    .await;
}

#[tokio::test]
async fn shell_starts_through_flag_form() {
    let dir = tmpdir();
    let mut cmd = Command::new(BIN);
    cmd.arg("--shell").arg(dir.path());
    let client: Client = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(cmd).expect("spawn shell server"),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("flag form starts");
    run_test(async move {
        let tools = client
            .list_tools(Default::default())
            .await
            .expect("list tools");
        assert!(tools.tools.iter().any(|t| t.name == "execute_command"));
    })
    .await;
}

#[tokio::test]
async fn shell_startup_without_directories_fails() {
    let output = Command::new(BIN)
        .arg("shell")
        .output()
        .await
        .expect("binary runs");
    assert!(!output.status.success(), "exit code is non-zero");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("DIR"),
        "clap explains the missing directory, got: {stderr}"
    );
}

#[tokio::test]
async fn shell_startup_with_inaccessible_directory_fails() {
    let output = Command::new(BIN)
        .args(["shell", "/nonexistent/does-not-exist"])
        .output()
        .await
        .expect("binary runs");
    assert!(!output.status.success(), "exit code is non-zero");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("are accessible"),
        "warns about inaccessible dirs, got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Server identity / capabilities via discover
// ---------------------------------------------------------------------------

#[tokio::test]
async fn discover_reports_identity_capabilities_and_version() {
    let dir = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    run_test(async move {
        let info = client.peer_info().expect("discover provides peer info");
        assert_eq!(
            info.protocol_version,
            rmcp::model::ProtocolVersion::V_2026_07_28,
            "negotiates the modern protocol version"
        );
        assert!(
            info.capabilities.tools.is_some(),
            "tools capability advertised"
        );
        assert!(
            info.capabilities.resources.is_none(),
            "no resources capability for the shell server"
        );
        assert!(
            info.capabilities.prompts.is_none(),
            "no prompts capability for the shell server"
        );

        let implementation = info
            .server_info
            .as_ref()
            .expect("server implementation identity provided");
        assert_eq!(implementation.name, "mcp-shell");
        assert_eq!(implementation.version, env!("CARGO_PKG_VERSION"));

        let instructions = info
            .instructions
            .as_deref()
            .expect("server instructions provided");
        assert!(
            instructions.contains("allowed directories"),
            "instructions explain the access model: {instructions}"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// tools/list contract
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tools_list_exposes_execute_command_with_schema_and_annotations() {
    let dir = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    run_test(async move {
        let tools = client
            .list_tools(Default::default())
            .await
            .expect("list tools");
        assert_eq!(tools.tools.len(), 1, "exactly one tool");

        let tool = &tools.tools[0];
        assert_eq!(tool.name, "execute_command");
        let description = tool.description.as_deref().unwrap_or_default();
        assert!(
            description.to_lowercase().contains("shell"),
            "describes the no-shell behavior: {description}"
        );

        let annotations = tool.annotations.as_ref().expect("annotations present");
        assert_eq!(annotations.read_only_hint, Some(false));
        assert_eq!(annotations.destructive_hint, Some(true));
        assert_eq!(annotations.idempotent_hint, Some(false));
        assert_eq!(annotations.open_world_hint, Some(true));

        let schema = tool.schema_as_json_value();
        let props = schema["properties"].as_object().expect("properties");
        assert!(props.contains_key("program"), "got: {schema}");
        assert!(props.contains_key("args"), "got: {schema}");
        assert!(props.contains_key("cwd"), "got: {schema}");
        assert!(props.contains_key("timeout_ms"), "got: {schema}");
        assert_eq!(props["program"]["type"], "string");
        assert_eq!(props["args"]["type"], "array");
        assert_eq!(props["args"]["default"], serde_json::json!([]));
        assert_eq!(props["timeout_ms"]["default"], 120_000);
        assert_eq!(props["timeout_ms"]["minimum"], 1);
        assert_eq!(props["timeout_ms"]["maximum"], 600_000);
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&"program".into()), "got: {schema}");

        // The declared output schema matches the structured result contract.
        let output_schema = tool
            .output_schema
            .clone()
            .expect("output schema declared")
            .as_ref()
            .clone();
        let out_props = output_schema["properties"]
            .as_object()
            .expect("output properties");
        for key in [
            "exit_code",
            "stdout",
            "stderr",
            "timed_out",
            "stdout_truncated",
            "stderr_truncated",
        ] {
            assert!(out_props.contains_key(key), "missing output field {key}");
        }

        // 2026-07-28 cache hints are preserved.
        assert_eq!(tools.ttl_ms, Some(0));
    })
    .await;
}

// ---------------------------------------------------------------------------
// Execution semantics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn executes_command_and_returns_stdout_with_exit_code() {
    let dir = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    let (program, args) = helper("echo", &["hello from helper"]);
    run_test(async move {
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": program, "args": args }),
        )
        .await;
        assert_eq!(result.is_error, Some(false), "got: {}", text(&result));
        let out = structured(&result);
        assert_eq!(out["exit_code"], 0);
        assert_eq!(out["timed_out"], false);
        assert_eq!(out["stdout_truncated"], false);
        let stdout = out["stdout"].as_str().unwrap();
        assert!(
            stdout.contains("hello from helper"),
            "captured stdout: {stdout}"
        );
        // Concise text content for text-only clients.
        let content = text(&result);
        assert!(content.contains("exit code: 0"), "got: {content}");
    })
    .await;
}

#[tokio::test]
async fn argv_entries_stay_distinct_and_unparsed() {
    let dir = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    let (program, args) = helper("echo", &["two words", "$HOME", "*", "a;b"]);
    run_test(async move {
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": program, "args": args }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        let stdout = structured(&result)["stdout"].as_str().unwrap().to_string();

        // Each argv entry arrives intact as one line: no shell splitting,
        // quoting, variable expansion, globbing, or command separators.
        assert!(stdout.contains("\ntwo words\n"), "got: {stdout}");
        assert!(
            stdout.contains("\n$HOME\n"),
            "variable not expanded: {stdout}"
        );
        assert!(stdout.contains("\n*\n"), "glob not expanded: {stdout}");
        assert!(
            stdout.contains("\na;b\n"),
            "separator not interpreted: {stdout}"
        );
    })
    .await;
}

#[tokio::test]
async fn non_zero_exit_is_a_normal_result() {
    let dir = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    let (program, args) = helper("exit", &["7"]);
    run_test(async move {
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": program, "args": args }),
        )
        .await;
        // A non-zero child exit is completed execution, not a tool failure.
        assert_eq!(result.is_error, Some(false), "got: {}", text(&result));
        let out = structured(&result);
        assert_eq!(out["exit_code"], 7);
        assert_eq!(out["timed_out"], false);
    })
    .await;
}

#[tokio::test]
async fn stderr_is_captured_separately() {
    let dir = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    let (program, args) = helper("stderr", &[]);
    run_test(async move {
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": program, "args": args }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        let out = structured(&result);
        assert!(
            out["stderr"]
                .as_str()
                .unwrap()
                .contains("helper stderr line"),
            "stderr captured: {}",
            out["stderr"]
        );
        assert!(
            !out["stdout"]
                .as_str()
                .unwrap()
                .contains("helper stderr line"),
            "stderr must not leak into stdout: {}",
            out["stdout"]
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// cwd access control
// ---------------------------------------------------------------------------

#[tokio::test]
async fn omitted_cwd_defaults_to_first_allowed_directory() {
    let dir = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    let (program, args) = helper("cwd", &[]);
    run_test(async move {
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": program, "args": args }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        let out = structured(&result);
        let cwd = out["stdout"].as_str().unwrap().to_string();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(
            reported_cwd(&cwd),
            canonical,
            "default cwd is the first allowed directory: {cwd}"
        );
    })
    .await;
}

#[tokio::test]
async fn valid_relative_cwd_resolves_against_first_allowed_directory() {
    let dir = tmpdir();
    std::fs::create_dir_all(join(dir.path(), "work")).unwrap();
    let client = connect_shell(&[dir.path()]).await;
    let (program, args) = helper("cwd", &[]);
    run_test(async move {
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": program, "args": args, "cwd": "work" }),
        )
        .await;
        assert_eq!(result.is_error, Some(false), "got: {}", text(&result));
        let out = structured(&result);
        let cwd = out["stdout"].as_str().unwrap().to_string();
        let canonical = std::fs::canonicalize(join(dir.path(), "work")).unwrap();
        assert_eq!(
            reported_cwd(&cwd),
            canonical,
            "resolved against the allowed root: {cwd}"
        );
    })
    .await;
}

#[tokio::test]
async fn out_of_scope_cwd_is_rejected() {
    let dir = tmpdir();
    let outside = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    let (program, args) = helper("cwd", &[]);
    run_test(async move {
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({
                "program": program,
                "args": args,
                "cwd": outside.path().to_str().unwrap()
            }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(
            text(&result).contains("Access denied"),
            "got: {}",
            text(&result)
        );
    })
    .await;
}

#[tokio::test]
async fn traversal_cwd_is_rejected() {
    let dir = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    let (program, args) = helper("cwd", &[]);
    run_test(async move {
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": program, "args": args, "cwd": "../escape" }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(
            text(&result).contains("Access denied"),
            "got: {}",
            text(&result)
        );
    })
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_escape_cwd_is_rejected() {
    let dir = tmpdir();
    let outside = tmpdir();
    std::os::unix::fs::symlink(outside.path(), join(dir.path(), "escape-link")).unwrap();
    let client = connect_shell(&[dir.path()]).await;
    let (program, args) = helper("cwd", &[]);
    run_test(async move {
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": program, "args": args, "cwd": "escape-link" }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(
            text(&result).contains("Access denied"),
            "got: {}",
            text(&result)
        );
    })
    .await;
}

#[tokio::test]
async fn file_as_cwd_is_rejected() {
    let dir = tmpdir();
    std::fs::write(join(dir.path(), "not-a-dir"), "x").unwrap();
    let client = connect_shell(&[dir.path()]).await;
    let (program, args) = helper("cwd", &[]);
    run_test(async move {
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": program, "args": args, "cwd": "not-a-dir" }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(
            text(&result).contains("not a directory"),
            "got: {}",
            text(&result)
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// Timeout
// ---------------------------------------------------------------------------

#[tokio::test]
async fn timeout_terminates_the_command_and_reports_it() {
    let dir = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    let (program, args) = helper("sleep", &["10000"]);
    run_test(async move {
        let start = std::time::Instant::now();
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": program, "args": args, "timeout_ms": 500 }),
        )
        .await;
        let elapsed = start.elapsed();

        assert_eq!(result.is_error, Some(false), "timeout is a normal outcome");
        let out = structured(&result);
        assert_eq!(out["timed_out"], true);
        assert_eq!(out["exit_code"], serde_json::Value::Null);
        // The child must actually have been terminated, not waited out.
        assert!(
            elapsed < Duration::from_secs(5),
            "returned promptly after terminating the child: {elapsed:?}"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// Output bounds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stdout_and_stderr_truncate_at_one_mi_b_with_flags() {
    let dir = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    let (program, args) = helper("big", &[]);
    run_test(async move {
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": program, "args": args }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        let out = structured(&result);
        assert_eq!(out["stdout_truncated"], true);
        let stdout = out["stdout"].as_str().unwrap();
        assert_eq!(
            stdout.len(),
            1024 * 1024,
            "exactly the capture limit is retained"
        );
        // The captured stream is the libtest banner followed by the helper's
        // payload; the retained bytes end inside the payload.
        assert!(stdout.ends_with('x'), "payload retained");
    })
    .await;

    let client2 = connect_shell(&[dir.path()]).await;
    let (program2, args2) = helper("bigstderr", &[]);
    run_test(async move {
        let result = call_tool(
            &client2,
            "execute_command",
            serde_json::json!({ "program": program2, "args": args2 }),
        )
        .await;
        let out = structured(&result);
        assert_eq!(out["stderr_truncated"], true);
        assert_eq!(out["stderr"].as_str().unwrap().len(), 1024 * 1024);
    })
    .await;
}

#[tokio::test]
async fn small_output_is_not_truncated() {
    let dir = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    let (program, args) = helper("echo", &["small payload"]);
    run_test(async move {
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": program, "args": args }),
        )
        .await;
        let out = structured(&result);
        assert_eq!(out["stdout_truncated"], false);
        assert_eq!(out["stderr_truncated"], false);
        assert!(
            out["stdout"].as_str().unwrap().contains("small payload"),
            "small output fully retained"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// Input validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn invalid_input_is_rejected() {
    let dir = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    run_test(async move {
        // Empty program.
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": "" }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains("non-empty"));

        // Whitespace-only program.
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": "   " }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));

        // timeout_ms below the minimum.
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": "x", "timeout_ms": 0 }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains("timeout_ms"));

        // timeout_ms above the maximum.
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": "x", "timeout_ms": 600_001 }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains("timeout_ms"));
    })
    .await;
}

#[tokio::test]
async fn unspawnable_program_is_a_tool_error() {
    let dir = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": "mcp-definitely-not-a-real-program-xyz" }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(
            text(&result).contains("Failed to spawn"),
            "got: {}",
            text(&result)
        );
    })
    .await;
}

#[tokio::test]
async fn timeout_boundaries_are_accepted() {
    let dir = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    let (program, args) = helper("echo", &["fast"]);

    // Minimum timeout: accepted (not rejected as below the minimum). The
    // child may or may not finish within 1 ms; either is a normal outcome.
    run_test(async move {
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": program, "args": args, "timeout_ms": 1 }),
        )
        .await;
        assert_eq!(result.is_error, Some(false), "got: {}", text(&result));
        let out = structured(&result);
        assert!(
            out["timed_out"] == true || out["stdout"].as_str().unwrap().contains("fast"),
            "completed or timed out, got: {out}"
        );
    })
    .await;

    // Maximum timeout: a fast command completes normally.
    let client2 = connect_shell(&[dir.path()]).await;
    let (program2, args2) = helper("echo", &["slow but allowed"]);
    run_test(async move {
        let result = call_tool(
            &client2,
            "execute_command",
            serde_json::json!({ "program": program2, "args": args2, "timeout_ms": 600_000 }),
        )
        .await;
        assert_eq!(result.is_error, Some(false), "got: {}", text(&result));
        let out = structured(&result);
        assert_eq!(out["timed_out"], false);
        assert!(out["stdout"].as_str().unwrap().contains("slow but allowed"));
    })
    .await;
}

#[tokio::test]
async fn wrong_or_missing_arguments_are_rejected() {
    let dir = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    run_test(async move {
        // Missing required `program`.
        let result = call_tool(&client, "execute_command", serde_json::json!({})).await;
        assert_eq!(result.is_error, Some(true), "got: {result:?}");
        assert!(text(&result).contains("failed to deserialize parameters"));

        // Wrong JSON types.
        for args in [
            serde_json::json!({ "program": 42 }),
            serde_json::json!({ "program": "x", "args": "not-an-array" }),
            serde_json::json!({ "program": "x", "timeout_ms": "fast" }),
            serde_json::json!({ "program": "x", "cwd": 7 }),
        ] {
            let result = call_tool(&client, "execute_command", args).await;
            assert_eq!(result.is_error, Some(true), "got: {result:?}");
            assert!(
                text(&result).contains("failed to deserialize parameters"),
                "got: {}",
                text(&result)
            );
        }
    })
    .await;
}

// ---------------------------------------------------------------------------
// Protocol hygiene
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tool_results_are_valid_json_rpc_despite_child_output() {
    let dir = tmpdir();
    let client = connect_shell(&[dir.path()]).await;
    let (program, args) = helper("echo", &["noise", "on", "stdout"]);
    run_test(async move {
        // A successful structured round-trip proves the child's output never
        // leaked into the protocol stream.
        let result = call_tool(
            &client,
            "execute_command",
            serde_json::json!({ "program": program, "args": args }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert!(
            structured(&result)["stdout"]
                .as_str()
                .unwrap()
                .contains("noise")
        );
    })
    .await;
}
