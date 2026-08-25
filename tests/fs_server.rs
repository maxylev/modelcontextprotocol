//! End-to-end tests for the filesystem server: spawns the real binary over
//! stdio and drives it through the MCP protocol with the rmcp client,
//! covering every tool and its parameters, mirroring the test coverage of the
//! reference TypeScript server.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{Client, call_tool, text};
use rmcp::{model::ContentBlock, service::ClientServiceExt, transport::TokioChildProcess};
use tempfile::TempDir;
use tokio::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_modelcontextprotocol");
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Start a filesystem server for the given allowed directories.
async fn connect(dirs: &[&Path]) -> Client {
    let mut cmd = Command::new(BIN);
    cmd.arg("filesystem");
    for dir in dirs {
        cmd.arg(dir);
    }
    let client: Client = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(cmd).expect("spawn filesystem server"),
            rmcp::service::ClientLifecycleMode::Discover {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("filesystem server starts");
    client
}

/// Run a test body with a deadline so a hung server fails the test.
async fn run_test<F>(future: F) -> F::Output
where
    F: std::future::Future<Output = ()>,
{
    tokio::time::timeout(REQUEST_TIMEOUT, future)
        .await
        .expect("test completed within timeout")
}

fn args(path: &str) -> serde_json::Value {
    serde_json::json!({ "path": path })
}

fn tmpdir() -> TempDir {
    tempfile::Builder::new()
        .prefix("mcp-fs-test-")
        .tempdir()
        .expect("create temp dir")
}

fn join(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}

// ---------------------------------------------------------------------------
// Startup validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn startup_without_directories_uses_current_directory() {
    let dir = tmpdir();
    let expected = dir.path().canonicalize().expect("canonical tempdir");
    let mut command = Command::new(BIN);
    command.arg("filesystem").current_dir(dir.path());
    let client: Client = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(command).expect("spawn filesystem server"),
            rmcp::service::ClientLifecycleMode::Discover {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("filesystem server starts");

    run_test(async move {
        let result = call_tool(&client, "list_allowed_directories", serde_json::json!({})).await;
        assert!(text(&result).contains(&expected.display().to_string()));
    })
    .await;
}

#[tokio::test]
async fn startup_with_only_inaccessible_directories_fails() {
    let output = Command::new(BIN)
        .args(["filesystem", "/nonexistent/does-not-exist"])
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

#[tokio::test]
async fn flag_form_is_equivalent() {
    let dir = tmpdir();
    let mut cmd = Command::new(BIN);
    cmd.arg("--filesystem").arg(dir.path());
    let client: Client = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(cmd).expect("spawn filesystem server"),
            rmcp::service::ClientLifecycleMode::Discover {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("flag form starts the server");
    let tools = client
        .list_tools(Default::default())
        .await
        .expect("list tools");
    assert!(tools.tools.iter().any(|t| t.name == "read_text_file"));
}

#[tokio::test]
async fn startup_without_arguments_fails() {
    let output = Command::new(BIN).output().await.expect("binary runs");
    assert!(!output.status.success(), "exit code is non-zero");
}

// ---------------------------------------------------------------------------
// Server identity / capabilities via discover
// ---------------------------------------------------------------------------

#[tokio::test]
async fn discover_reports_identity_capabilities_and_version() {
    let dir = tmpdir();
    let client = connect(&[dir.path()]).await;
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
            "no resources capability for the filesystem server"
        );
        assert!(
            info.capabilities.prompts.is_none(),
            "no prompts capability for the filesystem server"
        );

        let implementation = info
            .server_info
            .as_ref()
            .expect("server implementation identity provided");
        assert_eq!(implementation.name, "mcp-filesystem");
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
// tools/list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lists_all_filesystem_tools_with_annotations() {
    let dir = tmpdir();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        let tools = client
            .list_tools(Default::default())
            .await
            .expect("list tools");

        let expected = [
            "read_text_file",
            "read_file",
            "read_media_file",
            "read_multiple_files",
            "write_file",
            "edit_file",
            "create_directory",
            "list_directory",
            "list_directory_with_sizes",
            "directory_tree",
            "move_file",
            "search_files",
            "get_file_info",
            "list_allowed_directories",
        ];
        let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
        for name in expected {
            assert!(names.contains(&name), "missing tool {name}: {names:?}");
        }

        let by_name = |name: &str| tools.tools.iter().find(|t| t.name == name).unwrap();
        let read_only = by_name("read_text_file");
        assert_eq!(
            read_only.annotations.as_ref().unwrap().read_only_hint,
            Some(true)
        );
        assert_eq!(
            read_only.annotations.as_ref().unwrap().open_world_hint,
            Some(false)
        );

        let write = by_name("write_file");
        let ann = write.annotations.as_ref().unwrap();
        assert_eq!(ann.read_only_hint, Some(false));
        assert_eq!(ann.idempotent_hint, Some(true));
        assert_eq!(ann.destructive_hint, Some(true));
        assert_eq!(ann.open_world_hint, Some(false));

        let move_file = by_name("move_file");
        let ann = move_file.annotations.as_ref().unwrap();
        assert_eq!(ann.idempotent_hint, Some(false));
        assert_eq!(ann.destructive_hint, Some(true));

        // Head/tail described on read_text_file input schema.
        let schema = read_only.schema_as_json_value();
        assert!(schema["properties"]["head"].is_object());
        assert!(schema["properties"]["tail"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&"path".into())
        );

        // Cacheability hints required by the 2026-07-28 spec.
        assert_eq!(tools.ttl_ms, Some(0));
        assert_eq!(tools.cache_scope, Some(rmcp::model::CacheScope::Public));
    })
    .await;
}

// ---------------------------------------------------------------------------
// read_text_file
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_text_file_full_head_tail_and_errors() {
    let dir = tmpdir();
    std::fs::write(
        join(dir.path(), "sample.txt"),
        "line one\nline two\nline three\nline four\n",
    )
    .unwrap();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        // Full read.
        let result = call_tool(&client, "read_text_file", args("sample.txt")).await;
        assert_eq!(text(&result), "line one\nline two\nline three\nline four\n");

        // Head.
        let result = call_tool(
            &client,
            "read_text_file",
            serde_json::json!({ "path": "sample.txt", "head": 2 }),
        )
        .await;
        assert_eq!(text(&result), "line one\nline two");

        // Tail.
        let result = call_tool(
            &client,
            "read_text_file",
            serde_json::json!({ "path": "sample.txt", "tail": 2 }),
        )
        .await;
        assert_eq!(text(&result), "line three\nline four");

        // Head and tail together are rejected.
        let result = call_tool(
            &client,
            "read_text_file",
            serde_json::json!({ "path": "sample.txt", "head": 1, "tail": 1 }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains("both head and tail"));

        // Missing file is a tool error, not a crash.
        let result = call_tool(&client, "read_text_file", args("missing.txt")).await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains("missing.txt"));
    })
    .await;
}

#[tokio::test]
async fn deprecated_read_file_alias_works() {
    let dir = tmpdir();
    std::fs::write(join(dir.path(), "a.txt"), "hello").unwrap();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        let result = call_tool(&client, "read_file", args("a.txt")).await;
        assert_eq!(text(&result), "hello");
    })
    .await;
}

// ---------------------------------------------------------------------------
// read_multiple_files
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_multiple_files_tolerates_partial_failure() {
    let dir = tmpdir();
    std::fs::write(join(dir.path(), "one.txt"), "first").unwrap();
    std::fs::write(join(dir.path(), "two.txt"), "second").unwrap();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "read_multiple_files",
            serde_json::json!({ "paths": ["one.txt", "two.txt", "missing.txt"] }),
        )
        .await;
        let content = text(&result);
        assert!(content.contains("first"), "got: {content}");
        assert!(content.contains("second"), "got: {content}");
        assert!(content.contains("missing.txt: Error"), "got: {content}");
    })
    .await;
}

#[tokio::test]
async fn wrong_or_missing_arguments_are_rejected() {
    let dir = tmpdir();
    std::fs::write(join(dir.path(), "doc.txt"), "hello").unwrap();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        // Missing required `path`.
        for args in [serde_json::json!({}), serde_json::json!({ "head": 1 })] {
            let result = call_tool(&client, "read_text_file", args).await;
            assert_eq!(result.is_error, Some(true), "got: {result:?}");
            assert!(
                text(&result).contains("failed to deserialize parameters"),
                "got: {}",
                text(&result)
            );
        }

        // Wrong JSON type for `path`.
        let result = call_tool(&client, "read_text_file", serde_json::json!({ "path": 42 })).await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains("failed to deserialize parameters"));

        // Missing required `content` on write_file.
        let result = call_tool(
            &client,
            "write_file",
            serde_json::json!({ "path": "x.txt" }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains("failed to deserialize parameters"));
        assert!(!join(dir.path(), "x.txt").exists(), "nothing written");
    })
    .await;
}

// ---------------------------------------------------------------------------
// CLI server selection and option conflicts
// ---------------------------------------------------------------------------

/// Run the binary with the given arguments, asserting it exits non-zero, and
/// return its stderr.
async fn expect_cli_failure(args: &[&str]) -> String {
    let output = Command::new(BIN)
        .args(args)
        .output()
        .await
        .expect("binary runs");
    assert!(
        !output.status.success(),
        "expected non-zero exit for {args:?}, got {:?}",
        output.status.code()
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[tokio::test]
async fn fetch_options_with_unrelated_servers_are_rejected() {
    let dir = tmpdir();
    let other = tmpdir();
    for args in [
        vec![
            "--filesystem",
            dir.path().to_str().unwrap(),
            "--user-agent",
            "UA/1.0",
        ],
        vec![
            "--shell",
            dir.path().to_str().unwrap(),
            "--ignore-robots-txt",
        ],
        vec![
            "--ignore-robots-txt",
            "filesystem",
            dir.path().to_str().unwrap(),
        ],
        vec![
            "--proxy-url",
            "http://proxy",
            "memory",
            "--memory-file",
            other.path().to_str().unwrap(),
        ],
        vec!["--user-agent", "UA/1.0"],
    ] {
        let stderr = expect_cli_failure(&args).await;
        assert!(
            stderr.contains("exactly one server"),
            "explains the selection rule for {args:?}: {stderr}"
        );
    }
}

#[tokio::test]
async fn memory_file_with_unrelated_servers_is_rejected() {
    let dir = tmpdir();
    for args in [
        vec!["--fetch", "--memory-file", "unused.jsonl"],
        vec![
            "--shell",
            dir.path().to_str().unwrap(),
            "--memory-file",
            "unused.jsonl",
        ],
        vec!["--memory-file", "unused.jsonl", "fetch"],
        vec!["--memory-file", "unused.jsonl"],
        vec![
            "--filesystem",
            dir.path().to_str().unwrap(),
            "--memory-file",
            "unused.jsonl",
        ],
    ] {
        let stderr = expect_cli_failure(&args).await;
        assert!(
            stderr.contains("exactly one server"),
            "explains the selection rule for {args:?}: {stderr}"
        );
    }
}

#[tokio::test]
async fn multiple_server_selectors_are_rejected() {
    let dir = tmpdir();
    let other = tmpdir();
    for args in [
        vec!["--fetch", "--memory"],
        vec![
            "--filesystem",
            dir.path().to_str().unwrap(),
            "--shell",
            other.path().to_str().unwrap(),
        ],
        vec!["--memory", "--shell", dir.path().to_str().unwrap()],
    ] {
        let stderr = expect_cli_failure(&args).await;
        assert!(
            stderr.contains("exactly one server"),
            "explains the selection rule for {args:?}: {stderr}"
        );
    }
}

#[tokio::test]
async fn top_level_flags_after_a_subcommand_are_rejected() {
    let dir = tmpdir();
    // These never reach the normalization logic: clap itself rejects
    // top-level server flags placed after a subcommand.
    for args in [
        vec!["memory", "--fetch"],
        vec!["fetch", "--shell", dir.path().to_str().unwrap()],
        vec!["filesystem", dir.path().to_str().unwrap(), "--fetch"],
    ] {
        let _ = expect_cli_failure(&args).await;
    }
}

#[tokio::test]
async fn flag_server_without_directory_uses_current_directory() {
    let dir = tmpdir();
    let expected = dir.path().canonicalize().expect("canonical tempdir");
    let mut command = Command::new(BIN);
    command.arg("--filesystem").current_dir(dir.path());
    let client: Client = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(command).expect("spawn filesystem server"),
            rmcp::service::ClientLifecycleMode::Discover {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("filesystem server starts");

    run_test(async move {
        let result = call_tool(&client, "list_allowed_directories", serde_json::json!({})).await;
        assert!(text(&result).contains(&expected.display().to_string()));
    })
    .await;
}

// ---------------------------------------------------------------------------
// write_file / create_directory
// ---------------------------------------------------------------------------

#[tokio::test]
async fn write_file_creates_and_overwrites() {
    let dir = tmpdir();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "write_file",
            serde_json::json!({ "path": "new.txt", "content": "first write" }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            std::fs::read_to_string(join(dir.path(), "new.txt")).unwrap(),
            "first write"
        );

        let result = call_tool(
            &client,
            "write_file",
            serde_json::json!({ "path": "new.txt", "content": "second write" }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            std::fs::read_to_string(join(dir.path(), "new.txt")).unwrap(),
            "second write"
        );
    })
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn atomic_rewrites_preserve_existing_file_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tmpdir();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        // write_file over an existing 0600 file keeps it 0600.
        let private = join(dir.path(), "private.txt");
        std::fs::write(&private, "old content").unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600)).unwrap();
        let result = call_tool(
            &client,
            "write_file",
            serde_json::json!({ "path": "private.txt", "content": "new content" }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(std::fs::read_to_string(&private).unwrap(), "new content");
        let mode = std::fs::metadata(&private).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "write_file preserved 0600, got {mode:#o}");

        // edit_file over an existing 0755 file keeps it 0755.
        let executable = join(dir.path(), "executable.txt");
        std::fs::write(&executable, "alpha beta\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let result = call_tool(
            &client,
            "edit_file",
            serde_json::json!({
                "path": "executable.txt",
                "edits": [{"oldText": "beta", "newText": "BETA"}]
            }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            std::fs::read_to_string(&executable).unwrap(),
            "alpha BETA\n"
        );
        let mode = std::fs::metadata(&executable).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "edit_file preserved 0755, got {mode:#o}");
    })
    .await;
}

#[tokio::test]
async fn create_directory_nested_and_idempotent() {
    let dir = tmpdir();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        let result = call_tool(&client, "create_directory", args("a/b/c")).await;
        assert_eq!(result.is_error, Some(false));
        assert!(join(dir.path(), "a/b/c").is_dir());

        // Creating the same directory again succeeds silently.
        let result = call_tool(&client, "create_directory", args("a/b/c")).await;
        assert_eq!(result.is_error, Some(false));
    })
    .await;
}

// ---------------------------------------------------------------------------
// list_directory / list_directory_with_sizes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_directory_marks_files_and_dirs() {
    let dir = tmpdir();
    std::fs::write(join(dir.path(), "file.txt"), "x").unwrap();
    std::fs::create_dir(join(dir.path(), "subdir")).unwrap();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        let result = call_tool(&client, "list_directory", args("")).await;
        let content = text(&result);
        assert!(content.contains("[FILE] file.txt"), "got: {content}");
        assert!(content.contains("[DIR] subdir"), "got: {content}");
    })
    .await;
}

#[tokio::test]
async fn list_directory_empty_one_and_multiple_entries() {
    let dir = tmpdir();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        // Empty directory lists nothing.
        let result = call_tool(&client, "list_directory", args("")).await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(text(&result), "", "empty directory yields no entries");

        // A single entry lists exactly one line.
        std::fs::write(join(dir.path(), "solo.txt"), "x").unwrap();
        let result = call_tool(&client, "list_directory", args("")).await;
        assert_eq!(text(&result), "[FILE] solo.txt");

        // Multiple entries are sorted with [FILE]/[DIR] markers.
        std::fs::write(join(dir.path(), "zebra.txt"), "x").unwrap();
        std::fs::create_dir(join(dir.path(), "alpha")).unwrap();
        let result = call_tool(&client, "list_directory", args("")).await;
        let listing = text(&result);
        let lines: Vec<&str> = listing.lines().collect();
        assert_eq!(
            lines,
            ["[DIR] alpha", "[FILE] solo.txt", "[FILE] zebra.txt"],
            "entries sorted with markers"
        );
    })
    .await;
}

#[tokio::test]
async fn list_directory_with_sizes_sorts_and_summarizes() {
    let dir = tmpdir();
    std::fs::write(join(dir.path(), "big.txt"), "1234567890").unwrap();
    std::fs::write(join(dir.path(), "small.txt"), "a").unwrap();
    std::fs::create_dir(join(dir.path(), "subdir")).unwrap();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        // Default sort by name.
        let result = call_tool(
            &client,
            "list_directory_with_sizes",
            serde_json::json!({ "path": "" }),
        )
        .await;
        let content = text(&result);
        let big_pos = content.find("big.txt").unwrap();
        let small_pos = content.find("small.txt").unwrap();
        let sub_pos = content.find("subdir").unwrap();
        assert!(big_pos < small_pos, "sorted by name: {content}");
        assert!(sub_pos > small_pos, "directories listed too: {content}");
        assert!(
            content.contains("Total: 2 files, 1 directories"),
            "got: {content}"
        );
        assert!(content.contains("Combined size: 11 B"), "got: {content}");

        // Sort by size (descending).
        let result = call_tool(
            &client,
            "list_directory_with_sizes",
            serde_json::json!({ "path": "", "sortBy": "size" }),
        )
        .await;
        let content = text(&result);
        let big_pos = content.find("big.txt").unwrap();
        let small_pos = content.find("small.txt").unwrap();
        assert!(big_pos < small_pos, "sorted by size: {content}");

        // Invalid sortBy is rejected.
        let result = call_tool(
            &client,
            "list_directory_with_sizes",
            serde_json::json!({ "path": "", "sortBy": "mtime" }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains("sortBy"));
    })
    .await;
}

// ---------------------------------------------------------------------------
// directory_tree
// ---------------------------------------------------------------------------

#[tokio::test]
async fn directory_tree_returns_json_with_children() {
    let dir = tmpdir();
    std::fs::write(join(dir.path(), "root.txt"), "x").unwrap();
    std::fs::create_dir_all(join(dir.path(), "src/nested")).unwrap();
    std::fs::write(join(dir.path(), "src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(join(dir.path(), "src/nested/lib.rs"), "").unwrap();
    std::fs::write(join(dir.path(), "node_modules"), "").unwrap();
    std::fs::create_dir(join(dir.path(), "target")).unwrap();
    std::fs::write(join(dir.path(), "target/out.bin"), "b").unwrap();

    let client = connect(&[dir.path()]).await;
    run_test(async move {
        let result = call_tool(&client, "directory_tree", args("")).await;
        let content = text(&result);
        let tree: serde_json::Value = serde_json::from_str(&content).expect("valid JSON tree");

        fn find<'a>(tree: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
            tree.as_array().unwrap().iter().find(|e| e["name"] == name)
        }

        let root_file = find(&tree, "root.txt").expect("root.txt present");
        assert_eq!(root_file["type"], "file");
        assert!(root_file["children"].is_null(), "files have no children");

        let src = find(&tree, "src").expect("src present");
        assert_eq!(src["type"], "directory");
        assert!(src["children"].is_array(), "directories have children");
        let main_rs = find(&src["children"], "main.rs").expect("main.rs in src");
        assert_eq!(main_rs["type"], "file");
        let nested = find(&src["children"], "nested").expect("nested in src");
        assert_eq!(nested["children"].as_array().unwrap().len(), 1);

        // Exclude patterns prune the tree.
        let result = call_tool(
            &client,
            "directory_tree",
            serde_json::json!({ "path": "", "excludePatterns": ["target", "**/*.rs"] }),
        )
        .await;
        let content = text(&result);
        let tree: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(find(&tree, "target").is_none(), "excluded: {content}");
        assert!(find(&tree, "src").is_some());
        let src = find(&tree, "src").unwrap();
        assert!(
            find(&src["children"], "main.rs").is_none(),
            "glob-excluded: {content}"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// edit_file
// ---------------------------------------------------------------------------

#[tokio::test]
async fn edit_file_dry_run_previews_without_writing() {
    let dir = tmpdir();
    std::fs::write(join(dir.path(), "doc.txt"), "alpha\nbeta\ngamma\n").unwrap();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "edit_file",
            serde_json::json!({
                "path": "doc.txt",
                "edits": [{"oldText": "beta", "newText": "BETA"}],
                "dryRun": true
            }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        let content = text(&result);
        assert!(content.contains("```diff"), "got: {content}");
        assert!(content.contains("-beta"), "got: {content}");
        assert!(content.contains("+BETA"), "got: {content}");
        // File untouched in dry run mode.
        assert_eq!(
            std::fs::read_to_string(join(dir.path(), "doc.txt")).unwrap(),
            "alpha\nbeta\ngamma\n"
        );

        // Same edit applied for real.
        let result = call_tool(
            &client,
            "edit_file",
            serde_json::json!({
                "path": "doc.txt",
                "edits": [{"oldText": "beta", "newText": "BETA"}],
                "dryRun": false
            }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            std::fs::read_to_string(join(dir.path(), "doc.txt")).unwrap(),
            "alpha\nBETA\ngamma\n"
        );
    })
    .await;
}

#[tokio::test]
async fn edit_file_multiple_edits_and_whitespace_tolerance() {
    let dir = tmpdir();
    std::fs::write(
        join(dir.path(), "code.py"),
        "def main():\n    print('hi')\n    print('bye')\n",
    )
    .unwrap();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        // Whitespace-insensitive matching with indentation preserved.
        let result = call_tool(
            &client,
            "edit_file",
            serde_json::json!({
                "path": "code.py",
                "edits": [{"oldText": "print('hi')", "newText": "print('hello')"}],
                "dryRun": true
            }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        let content = text(&result);
        assert!(content.contains("+    print('hello')"), "got: {content}");

        // Multiple simultaneous edits. The earlier dry run did not write,
        // so the file still contains the original text.
        let result = call_tool(
            &client,
            "edit_file",
            serde_json::json!({
                "path": "code.py",
                "edits": [
                    {"oldText": "print('hi')", "newText": "print('HELLO')"},
                    {"oldText": "print('bye')", "newText": "print('GOODBYE')"}
                ]
            }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        let written = std::fs::read_to_string(join(dir.path(), "code.py")).unwrap();
        assert!(written.contains("print('HELLO')"), "got: {written}");
        assert!(written.contains("print('GOODBYE')"), "got: {written}");
    })
    .await;
}

#[tokio::test]
async fn edit_file_no_match_errors() {
    let dir = tmpdir();
    std::fs::write(join(dir.path(), "doc.txt"), "hello world").unwrap();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "edit_file",
            serde_json::json!({
                "path": "doc.txt",
                "edits": [{"oldText": "no such text", "newText": "x"}]
            }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains("Could not find exact match"));
    })
    .await;
}

// ---------------------------------------------------------------------------
// move_file
// ---------------------------------------------------------------------------

#[tokio::test]
async fn move_file_renames_and_moves() {
    let dir = tmpdir();
    std::fs::write(join(dir.path(), "source.txt"), "payload").unwrap();
    std::fs::create_dir(join(dir.path(), "dest")).unwrap();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "move_file",
            serde_json::json!({ "source": "source.txt", "destination": "dest/renamed.txt" }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert!(!join(dir.path(), "source.txt").exists());
        assert_eq!(
            std::fs::read_to_string(join(dir.path(), "dest/renamed.txt")).unwrap(),
            "payload"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// search_files
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_files_matches_globs_and_excludes() {
    let dir = tmpdir();
    std::fs::write(join(dir.path(), "a.rs"), "").unwrap();
    std::fs::write(join(dir.path(), "a.py"), "").unwrap();
    std::fs::create_dir_all(join(dir.path(), "src/sub")).unwrap();
    std::fs::write(join(dir.path(), "src/b.rs"), "").unwrap();
    std::fs::write(join(dir.path(), "src/c.rs"), "").unwrap();
    std::fs::write(join(dir.path(), "src/sub/d.rs"), "").unwrap();
    std::fs::write(join(dir.path(), "src/sub/skip.txt"), "").unwrap();
    std::fs::write(join(dir.path(), "src/sub/skip.rs"), "").unwrap();

    let client = connect(&[dir.path()]).await;
    run_test(async move {
        // Current directory only.
        let result = call_tool(
            &client,
            "search_files",
            serde_json::json!({ "path": "", "pattern": "*.rs" }),
        )
        .await;
        let content = text(&result).replace('\\', "/");
        assert!(content.contains("a.rs"), "got: {content}");
        assert!(
            !content.contains("src/b.rs"),
            "no recursion for *.rs: {content}"
        );

        // Recursive.
        let result = call_tool(
            &client,
            "search_files",
            serde_json::json!({ "path": "", "pattern": "**/*.rs" }),
        )
        .await;
        let content = text(&result).replace('\\', "/");
        for needle in [
            "a.rs",
            "src/b.rs",
            "src/c.rs",
            "src/sub/d.rs",
            "src/sub/skip.rs",
        ] {
            assert!(content.contains(needle), "missing {needle}: {content}");
        }

        // Exclusions remove matches.
        let result = call_tool(
            &client,
            "search_files",
            serde_json::json!({
                "path": "",
                "pattern": "**/*.rs",
                "excludePatterns": ["**/sub/**"]
            }),
        )
        .await;
        let content = text(&result).replace('\\', "/");
        assert!(!content.contains("src/sub"), "excluded: {content}");
        assert!(content.contains("src/b.rs"), "kept: {content}");

        // No matches.
        let result = call_tool(
            &client,
            "search_files",
            serde_json::json!({ "path": "", "pattern": "*.zzz" }),
        )
        .await;
        assert_eq!(text(&result), "No matches found");
    })
    .await;
}

// ---------------------------------------------------------------------------
// get_file_info
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_file_info_returns_metadata() {
    let dir = tmpdir();
    std::fs::write(join(dir.path(), "meta.txt"), "12345").unwrap();
    std::fs::create_dir(join(dir.path(), "subdir")).unwrap();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        let result = call_tool(&client, "get_file_info", args("meta.txt")).await;
        let content = text(&result);
        assert!(content.contains("size: 5"), "got: {content}");
        assert!(content.contains("isFile: true"), "got: {content}");
        assert!(content.contains("isDirectory: false"), "got: {content}");
        assert!(content.contains("permissions:"), "got: {content}");
        assert!(content.contains("modified:"), "got: {content}");

        let result = call_tool(&client, "get_file_info", args("subdir")).await;
        let content = text(&result);
        assert!(content.contains("isFile: false"), "got: {content}");
        assert!(content.contains("isDirectory: true"), "got: {content}");
    })
    .await;
}

// ---------------------------------------------------------------------------
// list_allowed_directories
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_allowed_directories_returns_roots() {
    let dir = tmpdir();
    let dir2 = tmpdir();
    let client = connect(&[dir.path(), dir2.path()]).await;
    run_test(async move {
        let result = call_tool(&client, "list_allowed_directories", serde_json::json!({})).await;
        let content = text(&result);
        assert!(content.contains("Allowed directories:"), "got: {content}");
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let canonical2 = std::fs::canonicalize(dir2.path()).unwrap();
        assert!(
            content.contains(&canonical.display().to_string()),
            "got: {content}"
        );
        assert!(
            content.contains(&canonical2.display().to_string()),
            "got: {content}"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// read_media_file
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_media_file_returns_typed_content() {
    let dir = tmpdir();
    // A minimal valid 1x1 PNG.
    let png = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
    )
    .unwrap();
    std::fs::write(join(dir.path(), "pixel.png"), png).unwrap();
    std::fs::write(join(dir.path(), "notes.txt"), "plain text").unwrap();

    let client = connect(&[dir.path()]).await;
    run_test(async move {
        let result = call_tool(&client, "read_media_file", args("pixel.png")).await;
        assert_eq!(result.is_error, Some(false));
        let image = result
            .content
            .iter()
            .find_map(|b| match b {
                ContentBlock::Image(i) => Some(i),
                _ => None,
            })
            .expect("image content block");
        assert_eq!(image.mime_type, "image/png");
        assert!(!image.data.is_empty());

        // Non-media files come back as an embedded resource.
        let result = call_tool(&client, "read_media_file", args("notes.txt")).await;
        assert_eq!(result.is_error, Some(false));
        let resource = result
            .content
            .iter()
            .find_map(|b| match b {
                ContentBlock::Resource(r) => Some(r),
                _ => None,
            })
            .expect("resource content block");
        let rmcp::model::ResourceContents::BlobResourceContents {
            uri,
            mime_type,
            blob,
            ..
        } = &resource.resource
        else {
            panic!("expected blob resource contents");
        };
        assert_eq!(mime_type.as_deref(), Some("text/plain"));
        assert!(uri.contains("notes.txt"), "got: {uri}");
        // Blob is base64 of the file contents.
        let decoded =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, blob).unwrap();
        assert_eq!(decoded, b"plain text");
    })
    .await;
}

// ---------------------------------------------------------------------------
// Security: access control
// ---------------------------------------------------------------------------

#[tokio::test]
async fn denies_paths_outside_allowed_directories() {
    let dir = tmpdir();
    let outside = tmpdir();
    std::fs::write(join(outside.path(), "secret.txt"), "top secret").unwrap();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        // Absolute path outside.
        let result = call_tool(
            &client,
            "read_text_file",
            args(outside.path().join("secret.txt").to_str().unwrap()),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(
            text(&result).contains("Access denied"),
            "got: {}",
            text(&result)
        );

        // Traversal escape.
        let result = call_tool(&client, "read_text_file", args("../secret.txt")).await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains("Access denied"));

        // Writing outside is denied too.
        let result = call_tool(
            &client,
            "write_file",
            serde_json::json!({
                "path": outside.path().join("evil.txt").to_str().unwrap(),
                "content": "evil"
            }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(!join(outside.path(), "evil.txt").exists());
    })
    .await;
}

// Symlink semantics only exist on unix; creating symlinks on Windows
// requires elevated privileges, so these tests are unix-only.
#[cfg(unix)]
#[tokio::test]
async fn denies_symlink_escapes() {
    let dir = tmpdir();
    let outside = tmpdir();
    std::fs::write(join(outside.path(), "secret.txt"), "top secret").unwrap();
    std::os::unix::fs::symlink(
        join(outside.path(), "secret.txt"),
        join(dir.path(), "link.txt"),
    )
    .unwrap();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        let result = call_tool(&client, "read_text_file", args("link.txt")).await;
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
async fn symlinks_inside_allowed_directories_are_allowed() {
    let dir = tmpdir();
    let inner = join(dir.path(), "inner");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(join(&inner, "real.txt"), "through link").unwrap();
    std::os::unix::fs::symlink(join(&inner, "real.txt"), join(dir.path(), "alias.txt")).unwrap();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        let result = call_tool(&client, "read_text_file", args("alias.txt")).await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(text(&result), "through link");
    })
    .await;
}

#[tokio::test]
async fn relative_paths_resolve_against_allowed_directory() {
    let dir = tmpdir();
    std::fs::write(join(dir.path(), "rel.txt"), "resolved").unwrap();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        let result = call_tool(&client, "read_text_file", args("rel.txt")).await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(text(&result), "resolved");
    })
    .await;
}

#[tokio::test]
async fn write_and_read_roundtrip() {
    let dir = tmpdir();
    let client = connect(&[dir.path()]).await;
    run_test(async move {
        call_tool(&client, "create_directory", args("docs")).await;
        let result = call_tool(
            &client,
            "write_file",
            serde_json::json!({ "path": "docs/roundtrip.txt", "content": "round trip content" }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        let result = call_tool(&client, "read_text_file", args("docs/roundtrip.txt")).await;
        assert_eq!(text(&result), "round trip content");
    })
    .await;
}
