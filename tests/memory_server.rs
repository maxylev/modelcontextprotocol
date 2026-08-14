//! End-to-end tests for the memory server: spawns the real binary over stdio,
//! drives it with the rmcp client, and verifies every knowledge-graph tool,
//! resource, and subscription behavior, mirroring the reference TypeScript
//! server's tests.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{Client, call_tool, text};
use rmcp::{
    model::{ReadResourceRequestParams, ServerNotification, SubscriptionFilter},
    service::{ClientLifecycleMode, ClientServiceExt},
    transport::TokioChildProcess,
};
use tempfile::TempDir;
use tokio::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_modelcontextprotocol");
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

fn structured(result: &rmcp::model::CallToolResult) -> serde_json::Value {
    result
        .structured_content
        .clone()
        .expect("result has structured content")
}

async fn connect_memory(memory_file: &Path) -> Client {
    let mut cmd = Command::new(BIN);
    cmd.arg("memory").arg("--memory-file").arg(memory_file);
    let client: Client = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(cmd).expect("spawn memory server"),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("memory server starts");
    client
}

/// Each test gets its own memory file so parallel tests never share state.
fn isolated_memory_file() -> (TempDir, std::path::PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("mcp-memory-test-")
        .tempdir()
        .expect("temp dir");
    let path = dir.path().join("memory.jsonl");
    (dir, path)
}

async fn run_test<F>(future: F) -> F::Output
where
    F: std::future::Future<Output = ()>,
{
    tokio::time::timeout(REQUEST_TIMEOUT, future)
        .await
        .expect("test completed within timeout")
}

fn alice() -> serde_json::Value {
    serde_json::json!({
        "name": "alice",
        "entityType": "person",
        "observations": ["speaks Spanish", "prefers morning meetings"]
    })
}

fn acme() -> serde_json::Value {
    serde_json::json!({
        "name": "acme",
        "entityType": "organization",
        "observations": ["sells widgets"]
    })
}

async fn seed(client: &Client) {
    call_tool(
        client,
        "create_entities",
        serde_json::json!({ "entities": [alice(), acme()] }),
    )
    .await;
}

async fn seed_with_relation(client: &Client) {
    seed(client).await;
    call_tool(
        client,
        "create_relations",
        serde_json::json!({
            "relations": [{ "from": "alice", "to": "acme", "relationType": "works_at" }]
        }),
    )
    .await;
}

// ---------------------------------------------------------------------------
// Server identity / capabilities via discover
// ---------------------------------------------------------------------------

#[tokio::test]
async fn discover_reports_identity_capabilities_and_version() {
    let (_dir, memory_file) = isolated_memory_file();
    let client = connect_memory(&memory_file).await;
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
            info.capabilities.prompts.is_none(),
            "no prompts capability for the memory server"
        );
        let resources = info
            .capabilities
            .resources
            .as_ref()
            .expect("resources capability advertised");
        assert_eq!(
            resources.subscribe,
            Some(true),
            "resource subscription advertised"
        );

        let implementation = info
            .server_info
            .as_ref()
            .expect("server implementation identity provided");
        assert_eq!(implementation.name, "mcp-memory");
        assert_eq!(implementation.version, env!("CARGO_PKG_VERSION"));

        let instructions = info
            .instructions
            .as_deref()
            .expect("server instructions provided");
        assert!(
            instructions.contains("knowledge graph"),
            "instructions explain the model: {instructions}"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// tools/list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lists_all_memory_tools_with_annotations() {
    let (_dir, memory_file) = isolated_memory_file();
    let client = connect_memory(&memory_file).await;
    run_test(async move {
        let tools = client
            .list_tools(Default::default())
            .await
            .expect("list tools");

        let expected = [
            "create_entities",
            "create_relations",
            "add_observations",
            "delete_entities",
            "delete_observations",
            "delete_relations",
            "read_graph",
            "search_nodes",
            "open_nodes",
        ];
        let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
        for name in expected {
            assert!(names.contains(&name), "missing tool {name}: {names:?}");
        }
        assert_eq!(tools.tools.len(), expected.len());

        let by_name = |name: &str| tools.tools.iter().find(|t| t.name == name).unwrap();
        let read_graph = by_name("read_graph");
        assert_eq!(
            read_graph.annotations.as_ref().unwrap().read_only_hint,
            Some(true)
        );
        assert_eq!(
            read_graph.annotations.as_ref().unwrap().idempotent_hint,
            Some(true)
        );
        let delete = by_name("delete_entities");
        let ann = delete.annotations.as_ref().unwrap();
        assert_eq!(ann.read_only_hint, Some(false));
        assert_eq!(ann.destructive_hint, Some(true));
        assert_eq!(ann.idempotent_hint, Some(true));
        let create = by_name("create_entities");
        let ann = create.annotations.as_ref().unwrap();
        assert_eq!(ann.read_only_hint, Some(false));
        assert_eq!(ann.destructive_hint, Some(false));

        // Schema uses camelCase names matching the reference server.
        let schema = create.schema_as_json_value();
        let entity_props = schema["$defs"]["Entity"]["properties"].as_object().unwrap();
        assert!(entity_props.contains_key("entityType"), "got: {schema}");
        assert!(entity_props["observations"]["type"] == "array");
        assert!(entity_props["observations"]["items"]["type"] == "string");

        // 2026-07-28 cache hints.
        assert_eq!(tools.ttl_ms, Some(0));
    })
    .await;
}

#[tokio::test]
async fn wrong_or_missing_arguments_are_rejected() {
    let (_dir, memory_file) = isolated_memory_file();
    let client = connect_memory(&memory_file).await;
    run_test(async move {
        // Missing required fields.
        for (name, args) in [
            ("create_entities", serde_json::json!({})),
            ("create_relations", serde_json::json!({})),
            ("add_observations", serde_json::json!({})),
            ("search_nodes", serde_json::json!({})),
            ("open_nodes", serde_json::json!({})),
            ("delete_entities", serde_json::json!({})),
        ] {
            let result = call_tool(&client, name, args).await;
            assert_eq!(result.is_error, Some(true), "{name}: {result:?}");
            assert!(
                text(&result).contains("failed to deserialize parameters"),
                "{name}: got {}",
                text(&result)
            );
        }

        // Wrong JSON types.
        for (name, args) in [
            (
                "create_entities",
                serde_json::json!({ "entities": "not-an-array" }),
            ),
            (
                "delete_entities",
                serde_json::json!({ "entityNames": [1, 2] }),
            ),
            ("search_nodes", serde_json::json!({ "query": 42 })),
        ] {
            let result = call_tool(&client, name, args).await;
            assert_eq!(result.is_error, Some(true), "{name}: {result:?}");
            assert!(
                text(&result).contains("failed to deserialize parameters"),
                "{name}: got {}",
                text(&result)
            );
        }

        // Nothing was written by the rejected calls.
        let graph = structured(&call_tool(&client, "read_graph", serde_json::json!({})).await);
        assert!(graph["entities"].as_array().unwrap().is_empty());
        assert!(graph["relations"].as_array().unwrap().is_empty());
    })
    .await;
}

// ---------------------------------------------------------------------------
// create / read / relations / observations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_entities_and_read_graph_roundtrip() {
    let (_dir, memory_file) = isolated_memory_file();
    let client = connect_memory(&memory_file).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "create_entities",
            serde_json::json!({ "entities": [alice(), acme()] }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        let added = structured(&result);
        assert_eq!(added["entities"].as_array().unwrap().len(), 2);

        // Duplicate names are ignored.
        let result = call_tool(
            &client,
            "create_entities",
            serde_json::json!({ "entities": [alice(), serde_json::json!({"name": "bob", "entityType": "person", "observations": []})] }),
        )
        .await;
        let added = structured(&result);
        assert_eq!(added["entities"].as_array().unwrap().len(), 1);
        assert_eq!(added["entities"][0]["name"], "bob");

        let result = call_tool(&client, "read_graph", serde_json::json!({})).await;
        assert_eq!(result.is_error, Some(false));
        let graph = structured(&result);
        assert_eq!(graph["entities"].as_array().unwrap().len(), 3);
        assert!(text(&result).contains("\"alice\""));
    })
    .await;
}

#[tokio::test]
async fn create_relations_skip_duplicates() {
    let (_dir, memory_file) = isolated_memory_file();
    let client = connect_memory(&memory_file).await;
    run_test(async move {
        seed(&client).await;
        let rel =
            serde_json::json!([{ "from": "alice", "to": "acme", "relationType": "works_at" }]);
        let result = call_tool(
            &client,
            "create_relations",
            serde_json::json!({ "relations": rel }),
        )
        .await;
        let added = structured(&result);
        assert_eq!(added["relations"].as_array().unwrap().len(), 1);

        // Same relation again is skipped.
        let result = call_tool(
            &client,
            "create_relations",
            serde_json::json!({ "relations": rel }),
        )
        .await;
        let added = structured(&result);
        assert_eq!(added["relations"].as_array().unwrap().len(), 0);

        let graph = structured(&call_tool(&client, "read_graph", serde_json::json!({})).await);
        assert_eq!(graph["relations"].as_array().unwrap().len(), 1);
    })
    .await;
}

#[tokio::test]
async fn add_observations_and_errors() {
    let (_dir, memory_file) = isolated_memory_file();
    let client = connect_memory(&memory_file).await;
    run_test(async move {
        seed(&client).await;

        let result = call_tool(
            &client,
            "add_observations",
            serde_json::json!({
                "observations": [{
                    "entityName": "alice",
                    "contents": ["likes tea", "likes tea"]
                }]
            }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        let results = structured(&result);
        assert_eq!(results["results"][0]["entityName"], "alice");
        // Like the reference server, duplicates within a single request are
        // all added (only pre-existing observations are filtered).
        assert_eq!(
            results["results"][0]["addedObservations"]
                .as_array()
                .unwrap()
                .len(),
            2,
            "duplicates within one call are added"
        );

        // Missing entity fails.
        let result = call_tool(
            &client,
            "add_observations",
            serde_json::json!({
                "observations": [{ "entityName": "ghost", "contents": ["x"] }]
            }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains("Entity with name ghost not found"));

        let graph = structured(&call_tool(&client, "read_graph", serde_json::json!({})).await);
        let alice = graph["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "alice")
            .unwrap();
        assert!(
            alice["observations"]
                .as_array()
                .unwrap()
                .contains(&"likes tea".into())
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// deletes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_entities_cascades_relations() {
    let (_dir, memory_file) = isolated_memory_file();
    let client = connect_memory(&memory_file).await;
    run_test(async move {
        seed(&client).await;
        let result = call_tool(
            &client,
            "delete_entities",
            serde_json::json!({ "entityNames": ["alice"] }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        let payload = structured(&result);
        assert_eq!(payload["success"], true);

        let graph = structured(&call_tool(&client, "read_graph", serde_json::json!({})).await);
        assert_eq!(graph["entities"].as_array().unwrap().len(), 1);
        assert!(
            graph["relations"].as_array().unwrap().is_empty(),
            "relations cascade-deleted"
        );

        // Silent when entity does not exist.
        let result = call_tool(
            &client,
            "delete_entities",
            serde_json::json!({ "entityNames": ["ghost"] }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
    })
    .await;
}

#[tokio::test]
async fn delete_observations_and_relations() {
    let (_dir, memory_file) = isolated_memory_file();
    let client = connect_memory(&memory_file).await;
    run_test(async move {
        seed_with_relation(&client).await;

        let result = call_tool(
            &client,
            "delete_observations",
            serde_json::json!({
                "deletions": [{ "entityName": "alice", "observations": ["speaks Spanish"] }]
            }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        let graph = structured(&call_tool(&client, "read_graph", serde_json::json!({})).await);
        let alice = graph["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "alice")
            .unwrap();
        assert!(
            !alice["observations"]
                .as_array()
                .unwrap()
                .contains(&"speaks Spanish".into())
        );
        assert!(
            alice["observations"]
                .as_array()
                .unwrap()
                .contains(&"prefers morning meetings".into())
        );

        let result = call_tool(
            &client,
            "delete_relations",
            serde_json::json!({
                "relations": [{ "from": "alice", "to": "acme", "relationType": "works_at" }]
            }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        let graph = structured(&call_tool(&client, "read_graph", serde_json::json!({})).await);
        assert!(graph["relations"].as_array().unwrap().is_empty());
    })
    .await;
}

// ---------------------------------------------------------------------------
// search / open
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_nodes_matches_names_types_and_observations() {
    let (_dir, memory_file) = isolated_memory_file();
    let client = connect_memory(&memory_file).await;
    run_test(async move {
        seed_with_relation(&client).await;

        // By name (case-insensitive).
        let result = call_tool(
            &client,
            "search_nodes",
            serde_json::json!({ "query": "ALICE" }),
        )
        .await;
        let graph = structured(&result);
        assert_eq!(graph["entities"][0]["name"], "alice");
        // Relations to nodes outside the result set are included.
        assert_eq!(graph["relations"].as_array().unwrap().len(), 1);

        // By observation content.
        let result = call_tool(
            &client,
            "search_nodes",
            serde_json::json!({ "query": "spanish" }),
        )
        .await;
        let graph = structured(&result);
        assert_eq!(graph["entities"].as_array().unwrap().len(), 1);
        assert_eq!(graph["entities"][0]["name"], "alice");

        // By entity type.
        let result = call_tool(
            &client,
            "search_nodes",
            serde_json::json!({ "query": "organization" }),
        )
        .await;
        let graph = structured(&result);
        assert_eq!(graph["entities"][0]["name"], "acme");

        // No matches.
        let result = call_tool(
            &client,
            "search_nodes",
            serde_json::json!({ "query": "nothing here" }),
        )
        .await;
        let graph = structured(&result);
        assert!(graph["entities"].as_array().unwrap().is_empty());
    })
    .await;
}

#[tokio::test]
async fn open_nodes_returns_requested_entities_and_relations() {
    let (_dir, memory_file) = isolated_memory_file();
    let client = connect_memory(&memory_file).await;
    run_test(async move {
        seed_with_relation(&client).await;

        let result = call_tool(
            &client,
            "open_nodes",
            serde_json::json!({ "names": ["acme", "ghost"] }),
        )
        .await;
        let graph = structured(&result);
        assert_eq!(graph["entities"].as_array().unwrap().len(), 1);
        assert_eq!(graph["entities"][0]["name"], "acme");
        assert_eq!(graph["relations"].as_array().unwrap().len(), 1);
        assert_eq!(graph["relations"][0]["from"], "alice");
    })
    .await;
}

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

#[tokio::test]
async fn knowledge_graph_resource_lists_and_reads() {
    let (_dir, memory_file) = isolated_memory_file();
    let client = connect_memory(&memory_file).await;
    run_test(async move {
        seed_with_relation(&client).await;

        let resources = client
            .list_resources(Default::default())
            .await
            .expect("list resources");
        assert_eq!(resources.resources.len(), 1);
        let resource = &resources.resources[0];
        assert_eq!(resource.uri, "memory://knowledge-graph");
        assert_eq!(resource.name, "knowledge-graph");
        assert_eq!(resource.mime_type.as_deref(), Some("application/json"));

        let result = client
            .read_resource(ReadResourceRequestParams::new("memory://knowledge-graph"))
            .await
            .expect("read resource");
        assert_eq!(result.contents.len(), 1);
        let rmcp::model::ResourceContents::TextResourceContents {
            text, mime_type, ..
        } = &result.contents[0]
        else {
            panic!("expected text resource");
        };
        assert_eq!(mime_type.as_deref(), Some("application/json"));
        let graph: serde_json::Value = serde_json::from_str(text).expect("valid JSON");
        assert_eq!(graph["entities"].as_array().unwrap().len(), 2);
        assert_eq!(graph["relations"].as_array().unwrap().len(), 1);

        // Unknown URIs are rejected.
        let result = client
            .read_resource(ReadResourceRequestParams::new("memory://other"))
            .await;
        assert!(result.is_err());
    })
    .await;
}

// ---------------------------------------------------------------------------
// Subscriptions (2026-07-28 modern flow)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resource_subscription_receives_update_notifications() {
    let (_dir, memory_file) = isolated_memory_file();
    let client = connect_memory(&memory_file).await;
    run_test(async move {
        // Subscribe to the knowledge-graph resource.
        let mut subscription = client
            .listen(
                SubscriptionFilter::builder()
                    .resource_subscription("memory://knowledge-graph")
                    .build(),
            )
            .await
            .expect("subscribe via subscriptions/listen");

        // Mutation tools trigger notifications/resources/updated. Poll the
        // notification stream and run the mutation concurrently so the
        // subscription is actively registered before the server emits; the
        // mutation branch yields and waits briefly first so the notification
        // future is guaranteed to be polled before create_entities runs.
        let (notified, mutation) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(subscription.next(), async {
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(50)).await;
                call_tool(
                    &client,
                    "create_entities",
                    serde_json::json!({ "entities": [alice()] }),
                )
                .await
            })
        })
        .await
        .expect("notification within timeout");
        assert_eq!(mutation.is_error, Some(false));

        let notification = notified
            .expect("next notification")
            .expect("notification stream alive");
        match notification {
            ServerNotification::ResourceUpdatedNotification(update) => {
                assert_eq!(update.params.uri, "memory://knowledge-graph");
            }
            other => panic!("expected resources/updated, got {other:?}"),
        }

        // Read-only tools do not notify.
        let result = call_tool(&client, "read_graph", serde_json::json!({})).await;
        assert_eq!(result.is_error, Some(false));
        let timed_out = tokio::time::timeout(Duration::from_millis(500), subscription.next())
            .await
            .is_err();
        assert!(timed_out, "no notification for read-only tools");

        let _ = subscription.cancel().await;
    })
    .await;
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

#[tokio::test]
async fn memory_persists_across_restarts() {
    let dir = tempfile::tempdir().expect("temp dir");
    let memory_file = dir.path().join("memory.jsonl");

    {
        let client = connect_memory(&memory_file).await;
        seed_with_relation(&client).await;
        client.cancel().await.expect("close server");
    }
    {
        let client = connect_memory(&memory_file).await;
        run_test(async move {
            let graph = structured(&call_tool(&client, "read_graph", serde_json::json!({})).await);
            assert_eq!(graph["entities"].as_array().unwrap().len(), 2);
            assert_eq!(graph["relations"].as_array().unwrap().len(), 1);
        })
        .await;
    }

    // The file is JSONL in the reference format.
    let content = std::fs::read_to_string(&memory_file).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines.iter().any(|l| l.starts_with("{\"type\":\"entity\"")));
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("{\"type\":\"relation\""))
    );
}

#[tokio::test]
async fn memory_file_env_var_is_honored() {
    let dir = tempfile::tempdir().expect("temp dir");
    let memory_file = dir.path().join("env-memory.jsonl");

    let mut cmd = Command::new(BIN);
    cmd.arg("memory").env("MEMORY_FILE_PATH", &memory_file);
    let client: Client = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(cmd).expect("spawn memory server"),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("memory server starts");
    run_test(async move {
        call_tool(
            &client,
            "create_entities",
            serde_json::json!({ "entities": [alice()] }),
        )
        .await;
        client.cancel().await.expect("close server");
    })
    .await;

    assert!(memory_file.exists(), "env var path used for storage");
}

// ---------------------------------------------------------------------------
// Startup / CLI forms
// ---------------------------------------------------------------------------

#[tokio::test]
async fn memory_flag_form_starts() {
    let dir = tempfile::tempdir().expect("temp dir");
    let memory_file = dir.path().join("flag-memory.jsonl");
    let mut cmd = Command::new(BIN);
    cmd.arg("--memory").arg("--memory-file").arg(&memory_file);
    let client: Client = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(cmd).expect("spawn memory server"),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("flag form starts");
    run_test(async move {
        call_tool(
            &client,
            "create_entities",
            serde_json::json!({ "entities": [alice()] }),
        )
        .await;
        let graph = structured(&call_tool(&client, "read_graph", serde_json::json!({})).await);
        assert_eq!(graph["entities"].as_array().unwrap().len(), 1);
        client.cancel().await.expect("close server");
    })
    .await;
    assert!(memory_file.exists(), "mutation writes the memory file");
}
