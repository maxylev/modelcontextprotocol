//! End-to-end tests for the Agent Skills server over its real stdio binary.

mod common;

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use common::{Client, call_tool, text};
use rmcp::{
    model::ProtocolVersion,
    service::{ClientLifecycleMode, ClientServiceExt},
    transport::TokioChildProcess,
};
use tempfile::TempDir;
use tokio::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_modelcontextprotocol");
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

fn tmpdir() -> TempDir {
    tempfile::Builder::new()
        .prefix("mcp-skills-test-")
        .tempdir()
        .expect("create tempdir")
}

fn write_skill(
    workspace: &Path,
    root: &str,
    directory: &str,
    name: &str,
    description: &str,
    body: &str,
) -> PathBuf {
    let skill_dir = workspace.join(root).join(directory);
    std::fs::create_dir_all(&skill_dir).expect("create skill directory");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
    )
    .expect("write SKILL.md");
    skill_dir
}

async fn connect_skills(workspace: &Path) -> Client {
    let mut command = Command::new(BIN);
    command.arg("skills").arg(workspace);
    ().serve_with_lifecycle(
        TokioChildProcess::new(command).expect("spawn skills server"),
        ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        },
    )
    .await
    .expect("skills server starts")
}

#[tokio::test]
async fn omitted_workspace_uses_current_directory() {
    let workspace = tmpdir();
    write_skill(
        workspace.path(),
        ".agents/skills",
        "demo",
        "demo",
        "Demo skill",
        "Use the demo.\n",
    );
    let mut command = Command::new(BIN);
    command.arg("skills").current_dir(workspace.path());
    let client: Client = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(command).expect("spawn skills server"),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("skills server starts");

    let tools = client.list_tools(Default::default()).await.unwrap().tools;
    let activate = tools
        .iter()
        .find(|tool| tool.name == "activate_skill")
        .expect("workspace skill tool is available");
    assert!(activate.description.as_deref().unwrap().contains("demo"));
}

async fn within_timeout(future: impl std::future::Future<Output = ()>) {
    tokio::time::timeout(REQUEST_TIMEOUT, future)
        .await
        .expect("test completed within timeout");
}

async fn cli_failure(args: &[String]) -> String {
    let output = Command::new(BIN)
        .args(args)
        .output()
        .await
        .expect("run binary");
    assert!(!output.status.success(), "expected failure for {args:?}");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[tokio::test]
async fn cli_subcommand_and_flag_forms_are_equivalent_without_shell_parsing() {
    let workspace = tmpdir();
    // A space catches regressions that build a command line string or invoke a shell.
    let workspace = workspace.path().join("workspace with spaces");
    std::fs::create_dir(&workspace).expect("create workspace");
    write_skill(
        &workspace,
        ".agents/skills",
        "demo",
        "demo",
        "Demo skill",
        "Use the demo.\n",
    );

    for args in [
        vec!["skills".into(), workspace.display().to_string()],
        vec!["--skills".into(), workspace.display().to_string()],
    ] {
        let mut command = Command::new(BIN);
        command.args(&args);
        let client: Client = ()
            .serve_with_lifecycle(
                TokioChildProcess::new(command).expect("spawn skills server"),
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                },
            )
            .await
            .expect("selected skills server starts");
        within_timeout(async move {
            let listed = client
                .list_tools(Default::default())
                .await
                .expect("list tools");
            assert_eq!(
                listed
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_ref())
                    .collect::<Vec<_>>(),
                ["activate_skill"]
            );
        })
        .await;
    }
}

#[tokio::test]
async fn cli_rejects_missing_or_invalid_workspaces_and_conflicting_options() {
    let workspace = tmpdir();
    let other = tmpdir();
    let workspace_path = workspace.path().display().to_string();
    for args in [
        vec!["skills".into()],
        vec![
            "skills".into(),
            workspace.path().join("missing").display().to_string(),
        ],
        vec![
            "--skills".into(),
            workspace_path.clone(),
            "--agents".into(),
            other.path().display().to_string(),
        ],
        vec![
            "--skills".into(),
            workspace_path.clone(),
            "--memory-file".into(),
            "unused.jsonl".into(),
        ],
        vec![
            "--skills".into(),
            workspace_path.clone(),
            "--user-agent".into(),
            "not-for-skills".into(),
        ],
        vec![
            "--skills".into(),
            workspace_path,
            "agents".into(),
            other.path().display().to_string(),
        ],
    ] {
        let stderr = cli_failure(&args).await;
        assert!(!stderr.is_empty(), "failure is explained for {args:?}");
    }
    let usage = cli_failure(&[]).await;
    assert!(usage.contains("skills [DIR]"), "usage: {usage}");
    assert!(usage.contains("agents [DIR]"), "usage: {usage}");
}

#[tokio::test]
async fn discover_reports_the_2026_skills_identity_and_concise_instructions() {
    let workspace = tmpdir();
    write_skill(
        workspace.path(),
        ".agents/skills",
        "demo",
        "demo",
        "Demo",
        "Do work.\n",
    );
    let client = connect_skills(workspace.path()).await;
    within_timeout(async move {
        let info = client.peer_info().expect("discover peer info");
        assert_eq!(info.protocol_version, ProtocolVersion::V_2026_07_28);
        assert!(
            info.capabilities.tools.is_some(),
            "tools capability advertised"
        );
        assert!(info.capabilities.prompts.is_none());
        assert!(info.capabilities.resources.is_none());
        let implementation = info.server_info.as_ref().expect("server identity");
        assert_eq!(implementation.name, "mcp-skills");
        assert_eq!(implementation.version, env!("CARGO_PKG_VERSION"));
        let instructions = info.instructions.as_deref().expect("instructions");
        assert!(
            instructions.contains("Activate an available skill"),
            "{instructions}"
        );
        assert!(
            instructions.len() < 200,
            "instructions remain concise: {instructions}"
        );
    })
    .await;
}

#[tokio::test]
async fn catalog_is_sorted_uses_agents_precedence_and_has_exact_tool_contract() {
    let workspace = tmpdir();
    let agent_pdf = write_skill(
        workspace.path(),
        ".agents/skills",
        "pdf-agent",
        "pdf",
        "Agent PDF",
        "AGENT PDF BODY\n",
    );
    write_skill(
        workspace.path(),
        ".claude/skills",
        "pdf-claude",
        "pdf",
        "Claude PDF",
        "CLAUDE PDF BODY\n",
    );
    write_skill(
        workspace.path(),
        ".claude/skills",
        "alpha",
        "alpha",
        "Alpha skill",
        "ALPHA BODY\n",
    );
    write_skill(
        workspace.path(),
        ".opencode/skills",
        "zeta",
        "zeta",
        "Zeta skill",
        "ZETA BODY\n",
    );
    let client = connect_skills(workspace.path()).await;
    within_timeout(async move {
        let tools = client
            .list_tools(Default::default())
            .await
            .expect("list tools");
        assert_eq!(tools.tools.len(), 1);
        assert_eq!(tools.ttl_ms, Some(0), "2026 cache hint");
        let tool = &tools.tools[0];
        assert_eq!(tool.name, "activate_skill");
        let description = tool.description.as_deref().expect("tool description");
        assert!(
            description.contains("- alpha: Alpha skill\n- pdf: Agent PDF\n- zeta: Zeta skill"),
            "{description}"
        );
        for body in [
            "AGENT PDF BODY",
            "CLAUDE PDF BODY",
            "ALPHA BODY",
            "ZETA BODY",
        ] {
            assert!(
                !description.contains(body),
                "catalog leaked body: {description}"
            );
        }
        let schema = tool.schema_as_json_value();
        assert_eq!(
            schema,
            serde_json::json!({
                "type": "object",
                "properties": { "name": { "type": "string", "enum": ["alpha", "pdf", "zeta"] } },
                "required": ["name"],
                "additionalProperties": false,
            })
        );
        let output = tool.output_schema.as_ref().expect("output schema").as_ref();
        for field in [
            "name",
            "description",
            "skill_dir",
            "instructions",
            "resources",
        ] {
            assert!(
                output["properties"].get(field).is_some(),
                "missing output {field}: {output:?}"
            );
        }
        let annotations = tool.annotations.as_ref().expect("annotations");
        assert_eq!(annotations.read_only_hint, Some(true));
        assert_eq!(annotations.destructive_hint, Some(false));
        assert_eq!(annotations.idempotent_hint, Some(true));
        assert_eq!(annotations.open_world_hint, Some(false));
        assert_eq!(agent_pdf, workspace.path().join(".agents/skills/pdf-agent"));
    })
    .await;
}

#[tokio::test]
async fn activation_returns_full_instructions_absolute_directory_and_sorted_paths_only() {
    let workspace = tmpdir();
    let skill = write_skill(
        workspace.path(),
        ".agents/skills",
        "pdf",
        "pdf",
        "Agent PDF",
        "FULL .agents INSTRUCTIONS\n",
    );
    std::fs::write(skill.join("z.txt"), "SECRET Z").expect("resource");
    std::fs::create_dir(skill.join("nested")).expect("nested resource dir");
    std::fs::write(skill.join("nested/a.txt"), "SECRET A").expect("resource");
    let expected_dir = std::fs::canonicalize(&skill)
        .expect("canonical skill dir")
        .display()
        .to_string();
    let client = connect_skills(workspace.path()).await;
    within_timeout(async move {
        let result = call_tool(
            &client,
            "activate_skill",
            serde_json::json!({"name": "pdf"}),
        )
        .await;
        assert_eq!(result.is_error, Some(false), "{}", text(&result));
        let activated = result
            .structured_content
            .as_ref()
            .expect("structured activation");
        assert_eq!(activated["name"], "pdf");
        assert_eq!(activated["description"], "Agent PDF");
        assert_eq!(activated["skill_dir"], expected_dir);
        assert_eq!(activated["instructions"], "FULL .agents INSTRUCTIONS\n");
        assert_eq!(
            activated["resources"],
            serde_json::json!(["nested/a.txt", "z.txt"])
        );
        assert!(
            !text(&result).contains("SECRET"),
            "text result must not load resources"
        );

        for arguments in [
            serde_json::json!({}),
            serde_json::json!({"name": "missing"}),
            serde_json::json!({"name": "pdf", "extra": true}),
        ] {
            let result = call_tool(&client, "activate_skill", arguments).await;
            assert_eq!(result.is_error, Some(true), "safe error: {result:?}");
            assert!(!text(&result).is_empty(), "safe error has text");
        }
    })
    .await;
}

#[tokio::test]
async fn empty_and_malformed_workspaces_do_not_expose_a_tool() {
    let empty = tmpdir();
    let client = connect_skills(empty.path()).await;
    within_timeout(async move {
        let info = client.peer_info().expect("discover peer info");
        assert!(
            info.capabilities.tools.is_none(),
            "no tool capability for empty catalog"
        );
        assert!(
            client
                .list_tools(Default::default())
                .await
                .expect("list tools")
                .tools
                .is_empty()
        );
    })
    .await;

    let workspace = tmpdir();
    write_skill(
        workspace.path(),
        ".agents/skills",
        "good",
        "good",
        "Good",
        "Good instructions.\n",
    );
    let bad = workspace.path().join(".claude/skills/bad");
    std::fs::create_dir_all(&bad).expect("bad skill dir");
    std::fs::write(bad.join("SKILL.md"), "not frontmatter").expect("malformed skill");
    let client = connect_skills(workspace.path()).await;
    within_timeout(async move {
        let tools = client
            .list_tools(Default::default())
            .await
            .expect("list tools");
        assert_eq!(
            tools.tools[0].schema_as_json_value()["properties"]["name"]["enum"],
            serde_json::json!(["good"])
        );
    })
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_aliases_and_escapes_are_not_catalog_entries() {
    use std::os::unix::fs::symlink;

    let workspace = tmpdir();
    let real = write_skill(
        workspace.path(),
        ".agents/skills",
        "real",
        "real",
        "Real",
        "Real instructions.\n",
    );
    symlink(&real, workspace.path().join(".agents/skills/alias")).expect("alias skill");
    let outside = tmpdir();
    let escaped = write_skill(
        outside.path(),
        ".agents/skills",
        "escape",
        "escape",
        "Escape",
        "Escape instructions.\n",
    );
    std::fs::create_dir_all(workspace.path().join(".opencode/skills")).expect("create escape root");
    symlink(
        &escaped,
        workspace.path().join(".opencode/skills/escape-link"),
    )
    .expect("escape skill");
    let client = connect_skills(workspace.path()).await;
    within_timeout(async move {
        let tool = &client
            .list_tools(Default::default())
            .await
            .expect("list tools")
            .tools[0];
        assert_eq!(
            tool.schema_as_json_value()["properties"]["name"]["enum"],
            serde_json::json!(["real"])
        );
    })
    .await;
}

#[tokio::test]
async fn older_explicit_discover_protocol_is_rejected() {
    let workspace = tmpdir();
    write_skill(
        workspace.path(),
        ".agents/skills",
        "demo",
        "demo",
        "Demo",
        "Do work.\n",
    );
    let mut command = Command::new(BIN);
    command.arg("skills").arg(workspace.path());
    let result: Result<Client, _> = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(command).expect("spawn skills server"),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2025_11_25],
            },
        )
        .await;
    assert!(
        result.is_err(),
        "the skills server only accepts the 2026-07-28 Discover lifecycle"
    );

    let mut command = Command::new(BIN);
    command.arg("skills").arg(workspace.path());
    let legacy: Result<Client, _> = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(command).expect("spawn skills server"),
            ClientLifecycleMode::Initialize,
        )
        .await;
    assert!(
        legacy.is_err(),
        "the legacy initialize lifecycle is rejected"
    );
}
