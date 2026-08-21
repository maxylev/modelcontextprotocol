//! Deterministic MCP integration tests for the real `skills` process over
//! stdio, using the shared acceptance fixture (release-audit +
//! reviewer-guidance skills, duplicate/malformed isolation).

mod support;

use rmcp::model::ProtocolVersion;
use serde_json::{Value, json};

use support::fixture::{RELEASE_CHANNEL, RELEASE_MARKER, RESOURCE_RELATIVE, Workspace};
use support::mcp_client::{self, connect_skills, list_tool_names, text};

#[tokio::test]
async fn skills_reports_2026_identity_and_exact_surface() {
    let workspace = Workspace::new(mcp_client::BIN);
    let client = connect_skills(&workspace.root, None).await;
    let info = client.peer_info().expect("discover peer info");
    assert_eq!(info.protocol_version, ProtocolVersion::V_2026_07_28);
    let implementation = info.server_info.as_ref().expect("server identity");
    assert_eq!(implementation.name, "mcp-skills");
    assert_eq!(implementation.version, env!("CARGO_PKG_VERSION"));
    assert!(info.capabilities.tools.is_some());
    assert!(info.capabilities.resources.is_none());
    let instructions = info.instructions.as_deref().expect("instructions");
    assert!(
        instructions.contains("Activate an available skill"),
        "{instructions}"
    );
    assert_eq!(list_tool_names(&client).await, ["activate_skill"]);

    let listed = client
        .list_tools(Default::default())
        .await
        .expect("tools/list");
    assert_eq!(listed.ttl_ms, Some(0));
    assert_eq!(listed.cache_scope, Some(rmcp::model::CacheScope::Private));
    assert_eq!(listed.tools.len(), 1);
    let tool = &listed.tools[0];
    assert_eq!(tool.name, "activate_skill");

    // The hidden resource markers must never leak into the tool catalog.
    let catalog = serde_json::to_string(tool).unwrap_or_default();
    assert!(
        !catalog.contains(RELEASE_CHANNEL),
        "catalog leaked resource marker"
    );
    assert!(
        !catalog.contains(RELEASE_MARKER),
        "catalog leaked resource marker"
    );
}

#[tokio::test]
async fn catalog_has_release_audit_once_and_duplicate_malformed_are_isolated() {
    let workspace = Workspace::new(mcp_client::BIN);
    let client = connect_skills(&workspace.root, None).await;
    let tools = mcp_client::list_tools(&client).await;
    assert_eq!(tools.len(), 1);
    let tool = &tools[0];
    let schema = tool.schema_as_json_value();
    let enum_names = schema["properties"]["name"]["enum"]
        .as_array()
        .expect("enum array")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert_eq!(enum_names, ["release-audit", "reviewer-guidance"]);
    let description = tool.description.as_deref().expect("description");
    // Canonical .agents copy wins over the .claude duplicate; the duplicate
    // body/description must not surface.
    assert!(
        description.contains("release-audit: Use this skill for release-readiness"),
        "canonical description missing: {description}"
    );
    assert!(
        !description.contains("DUPLICATE"),
        "lower-precedence duplicate leaked into the catalog"
    );
    assert!(
        !description.contains("broken"),
        "malformed skill leaked into the catalog"
    );
    assert!(
        schema["properties"]["name"]["enum"]
            .as_array()
            .unwrap()
            .len()
            == 2
    );
    assert!(tree_schema_additional_properties_false(&schema));
}

fn tree_schema_additional_properties_false(schema: &Value) -> bool {
    schema.get("additionalProperties") == Some(&Value::Bool(false))
}

#[tokio::test]
async fn activate_release_audit_returns_instructions_skill_dir_and_resource_manifest() {
    let workspace = Workspace::new(mcp_client::BIN);
    let client = connect_skills(&workspace.root, None).await;
    let result =
        mcp_client::call_tool(&client, "activate_skill", json!({"name": "release-audit"})).await;
    assert_eq!(result.is_error, Some(false), "{}", text(&result));
    let activated = mcp_client::structured(&result);
    assert_eq!(activated["name"], "release-audit");
    assert_eq!(
        activated["description"],
        "Use this skill for release-readiness, release validation, or pre-deployment engineering audits."
    );
    assert_eq!(
        activated["skill_dir"],
        workspace
            .skill_dir()
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
    );
    let instructions = activated["instructions"].as_str().expect("instructions");
    assert!(
        instructions.contains("references/release-contract.md"),
        "resource instructions missing: {instructions}"
    );
    assert!(instructions.contains("authoritative"), "{instructions}");
    assert!(
        activated["resources"]
            .as_array()
            .expect("resources array")
            .iter()
            .any(|resource| resource.as_str() == Some(RESOURCE_RELATIVE)),
        "resource manifest missing references/release-contract.md: {activated}"
    );

    // Activation must load instructions + manifest, not the resource body.
    let serialized = serde_json::to_string(&activated).unwrap_or_default();
    assert!(
        !serialized.contains(RELEASE_CHANNEL),
        "resource body leaked into activation"
    );
    assert!(
        !serialized.contains(RELEASE_MARKER),
        "resource body leaked into activation"
    );
    assert!(!text(&result).contains(RELEASE_CHANNEL));
}

#[tokio::test]
async fn activation_schema_constrains_names_and_unknown_skill_fails_safely() {
    let workspace = Workspace::new(mcp_client::BIN);
    let client = connect_skills(&workspace.root, None).await;
    let tools = mcp_client::list_tools(&client).await;
    let schema = tools[0].schema_as_json_value();
    assert_eq!(schema["properties"]["name"]["type"], "string", "{schema}");
    assert_eq!(
        schema["properties"]["name"]["enum"],
        json!(["release-audit", "reviewer-guidance"])
    );
    assert_eq!(schema["required"], json!(["name"]));
    assert_eq!(schema["additionalProperties"], json!(false));

    for arguments in [
        json!({"name": "no-such-skill"}),
        json!({}),
        json!({"name": "release-audit", "extra": true}),
    ] {
        let result = mcp_client::call_tool(&client, "activate_skill", arguments.clone()).await;
        assert_eq!(
            result.is_error,
            Some(true),
            "expected error for {arguments:?}: {}",
            text(&result)
        );
        assert!(!text(&result).is_empty(), "error has text");
    }
    let result =
        mcp_client::call_tool(&client, "activate_skill", json!({"name": "no-such-skill"})).await;
    assert!(
        mcp_client::text(&result).contains("unknown skill"),
        "{}",
        mcp_client::text(&result)
    );
}

#[tokio::test]
async fn reviewer_guidance_activates_and_is_available_to_agents_workspace() {
    let workspace = Workspace::new(mcp_client::BIN);
    let client = connect_skills(&workspace.root, None).await;
    let result = mcp_client::call_tool(
        &client,
        "activate_skill",
        json!({"name": "reviewer-guidance"}),
    )
    .await;
    assert_eq!(result.is_error, Some(false), "{}", text(&result));
    let activated = mcp_client::structured(&result);
    assert_eq!(activated["name"], "reviewer-guidance");
    assert_eq!(activated["resources"], json!([]));
    assert!(
        activated["instructions"]
            .as_str()
            .expect("instructions")
            .contains("Review the requested code carefully")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn resource_symlink_escape_is_rejected_at_activation_without_breaking_catalog() {
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let workspace = Workspace::new(mcp_client::BIN);
    let outside = tempdir().expect("outside dir");
    std::fs::write(outside.path().join("secret"), "secret").expect("write secret");
    let skill_dir = workspace.skill_dir();
    symlink(outside.path().join("secret"), skill_dir.join("escape.txt")).expect("symlink");

    let client = connect_skills(&workspace.root, None).await;
    // The skill remains a catalog entry; the manifest rejects the escape on
    // activation (manifest is computed lazily).
    let tools = mcp_client::list_tools(&client).await;
    assert!(
        tools[0].schema_as_json_value()["properties"]["name"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "release-audit")
    );
    let result =
        mcp_client::call_tool(&client, "activate_skill", json!({"name": "release-audit"})).await;
    assert_eq!(
        result.is_error,
        Some(true),
        "escaped resource must be rejected: {}",
        text(&result)
    );
    assert!(
        mcp_client::text(&result).contains("escapes skill directory"),
        "{}",
        mcp_client::text(&result)
    );
}

#[tokio::test]
async fn filesystem_identity_and_read_tools_are_available_for_skill_resources() {
    let workspace = Workspace::new(mcp_client::BIN);
    let client = support::mcp_client::connect_filesystem(&workspace.root, None).await;
    let info = client.peer_info().expect("discover peer info");
    assert_eq!(
        info.server_info.as_ref().expect("identity").name,
        "mcp-filesystem"
    );
    let names = list_tool_names(&client).await;
    // The filesystem read surface must come from the real catalog; the
    // acceptance test resolves the exact read tool dynamically from this list.
    for expected in ["read_text_file", "read_file", "list_directory"] {
        assert!(
            names.iter().any(|name| name == expected),
            "missing {expected} in filesystem catalog: {names:?}"
        );
    }
}
