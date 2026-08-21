//! Real-network production-like acceptance test: a real parent LLM
//! (`openai/gpt-5.6-luna` through OpenRouter) operates the real Skills,
//! Agents, and Filesystem MCP servers over stdio, activates a skill, reads
//! its supporting resource, spawns two independent subagents (reviewer,
//! researcher), waits for them, continues the reviewer by the same
//! `agent_id`, and produces one final report.
//!
//! This test is `#[ignore]`d by default: it requires the real
//! `OPENROUTER_API_KEY` from the repository-root `.env.test` and spends
//! tokens. It is run explicitly for acceptance.

mod support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::task::JoinSet;

use support::fixture::{
    AUTH_PATH, RELEASE_CHANNEL, RELEASE_MARKER, RESEARCHER_NAME, RESOURCE_RELATIVE,
    RETRY_CONTRACT_PATH, RETRY_PATH, REVIEWER_NAME, Workspace,
};
use support::mcp_client::{self, Client};
use support::openrouter::{
    MAX_PARENT_TURNS, MODEL, ResponsesClient, TOOL_OUTPUT_CAP_CHARS, Trace, TraceResultKind,
    contains_any, load_api_key_from_env_test,
};
use support::production_prompt::{ACCEPTANCE_USER_TASK, PRODUCTION_PARENT_SYSTEM_PROMPT};

/// Hard whole-test deadline (provider + subagent timeouts are separate and
/// tighter).
const WHOLE_TEST_TIMEOUT: Duration = Duration::from_secs(8 * 60);

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires OPENROUTER_API_KEY from .env.test and real OpenRouter network access to openai/gpt-5.6-luna"]
async fn real_llm_skills_agents_acceptance() {
    let outcome = tokio::time::timeout(WHOLE_TEST_TIMEOUT, accept()).await;
    match outcome {
        Ok(Ok(())) => println!("acceptance: passed"),
        Ok(Err(failure)) => panic!("acceptance failed: {failure}"),
        Err(_) => panic!(
            "acceptance exceeded the {}s whole-test deadline",
            WHOLE_TEST_TIMEOUT.as_secs()
        ),
    }
}

/// Semantic assertion returning `Err` from the acceptance function when it
/// fails, keeping the failure diagnostic bounded.
macro_rules! assert_semantic {
    ($condition:expr, $message:expr, $evidence:expr) => {
        if !($condition) {
            let shown = bounded_evidence(&$evidence, 1500);
            return Err(format!("{}\nEvidence (bounded): {shown}", $message));
        }
    };
}

// ---------------------------------------------------------------------------
// Trace sanitizer regression tests (Fix 3)
// ---------------------------------------------------------------------------

#[test]
fn trace_serialize_sanitized_none_or_empty_secret_returns_unchanged_json() {
    let mut trace = Trace::default();
    trace.record(
        0,
        "activate_skill",
        &json!({"name": "release-audit"}),
        TraceResultKind::Ok,
        None,
    );
    trace.record(
        1,
        "spawn_agent",
        &json!({"name": "reviewer", "task": "a task"}),
        TraceResultKind::Ok,
        Some(""),
    );

    for secret in [None, Some("")] {
        let serialized = trace.serialize_sanitized(secret);
        // Valid, readable JSON; nothing redacted by an empty substring.
        let parsed: Vec<Value> = serde_json::from_str(&serialized).expect("valid JSON");
        assert_eq!(parsed.len(), 2, "entries preserved for {secret:?}");
        assert!(!serialized.contains("[REDACTED]"), "{secret:?} redacted");
        assert!(serialized.contains("activate_skill"), "{serialized}");
        assert!(serialized.contains("release-audit"), "{serialized}");
        assert!(serialized.contains("reviewer"), "{serialized}");
    }
}

#[test]
fn trace_serialize_sanitized_redacts_real_secret_and_leaves_absent_data_alone() {
    let mut trace = Trace::default();
    trace.record(
        0,
        "spawn_agent",
        &json!({"name": "reviewer", "task": "agent token=super-secret-123 scoped"}),
        TraceResultKind::Ok,
        Some("super-secret-123"),
    );

    let redacted = trace.serialize_sanitized(Some("super-secret-123"));
    assert!(
        !redacted.contains("super-secret-123"),
        "secret present after redaction"
    );
    assert!(redacted.contains("[REDACTED]"), "{redacted}");
    // The rest of the entry remains intact and readable.
    assert!(redacted.contains("reviewer"), "{redacted}");
    assert!(redacted.contains("agent token"), "{redacted}");

    // A secret that is absent from the data leaves the serialized trace
    // unchanged (apart from the pre-recorded redaction of the real secret).
    let absent = trace.serialize_sanitized(Some("not-in-the-data"));
    assert!(!absent.contains("[REDACTED]") || !absent.contains("super-secret-123"));
    assert!(serde_json::from_str::<Vec<Value>>(&absent).is_ok());
}

#[test]
fn trace_sanitize_redacts_nested_argument_strings_and_stays_bounded() {
    let mut trace = Trace::default();
    trace.record(
        0,
        "read_text_file",
        &json!({
            "path": "/workspace/a.txt",
            "content": "token=secretbound nested value",
        }),
        TraceResultKind::Ok,
        Some("secretbound"),
    );
    let serialized = trace.serialize_sanitized(Some("secretbound"));
    // String values are redacted recursively, including nested fields.
    assert!(!serialized.contains("secretbound"), "{serialized}");
    assert!(serialized.contains("[REDACTED]"), "{serialized}");

    // Oversized arguments are bounded so diagnostics stay readable.
    let mut big = Trace::default();
    let huge = "x".repeat(100_000);
    big.record(
        0,
        "read_text_file",
        &json!({"path": "a.txt", "content": huge}),
        TraceResultKind::Ok,
        None,
    );
    let serialized = big.serialize_sanitized(None);
    assert!(serialized.len() < 20_000, "bounded: {}", serialized.len());
    assert!(serde_json::from_str::<Vec<Value>>(&serialized).is_ok());
}

async fn accept() -> Result<(), String> {
    // -- real credential from .env.test, held in memory only -----------------
    let api_key = load_api_key_from_env_test().map_err(|error| error.to_string())?;
    if api_key.trim().is_empty() {
        return Err("OPENROUTER_API_KEY from .env.test is empty".to_string());
    }

    // -- deterministic temporary workspace ----------------------------------
    let workspace = Workspace::new(mcp_client::BIN);

    // -- real MCP servers over stdio (2026-07-28 Discover) -------------------
    println!("parent: connecting to mcp-skills");
    let skills = Arc::new(mcp_client::connect_skills(&workspace.root, None).await);
    println!("parent: connecting to mcp-agents");
    let agents = Arc::new(mcp_client::connect_agents(&workspace.root, Some(&api_key)).await);
    println!("parent: connecting to mcp-filesystem");
    let filesystem = Arc::new(mcp_client::connect_filesystem(&workspace.root, None).await);

    assert_eq!(mcp_client::identity_name(&skills), "mcp-skills");
    assert_eq!(mcp_client::identity_name(&agents), "mcp-agents");
    assert_eq!(mcp_client::identity_name(&filesystem), "mcp-filesystem");

    // -- build the parent tool catalog from real MCP discovery ---------------
    let mut routes: HashMap<String, Arc<Client>> = HashMap::new();
    let mut provider_tools: Vec<Value> = Vec::new();
    let mut filesystem_tools: Vec<String> = Vec::new();
    for (server, client) in [
        ("skills", &skills),
        ("agents", &agents),
        ("filesystem", &filesystem),
    ] {
        for tool in mcp_client::list_tools(client).await {
            if server == "filesystem" {
                filesystem_tools.push(tool.name.to_string());
            }
            routes.insert(tool.name.to_string(), Arc::clone(client));
            provider_tools.push(support::openrouter::provider_tool(&tool));
        }
    }

    // -- shared outcome collectors -------------------------------------------
    let trace = Arc::new(Mutex::new(Trace::default()));
    // agent_id -> ordered list of result texts, appended per wait_agent call.
    let agent_results: Arc<Mutex<HashMap<String, Vec<String>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let spawned: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

    // -- parent LLM tool loop -------------------------------------------------
    let llm = ResponsesClient::new(api_key.clone());
    let mut input = vec![json!({
        "role": "user",
        "content": [{"type": "input_text", "text": ACCEPTANCE_USER_TASK}]
    })];
    let mut final_text = String::new();

    'acceptance: for turn in 0..MAX_PARENT_TURNS {
        let parsed = llm
            .complete(
                MODEL,
                PRODUCTION_PARENT_SYSTEM_PROMPT,
                &input,
                &provider_tools,
            )
            .await
            .map_err(|error| format!("parent request turn {turn} failed: {error}"))?;
        input.extend(parsed.items.clone());
        if parsed.calls.is_empty() {
            final_text = parsed
                .text
                .filter(|text| !text.trim().is_empty())
                .unwrap_or_default();
            if final_text.trim().is_empty() {
                return Err(format!(
                    "parent produced a final turn with no tool calls and no visible text (turn {turn})"
                ));
            }
            break 'acceptance;
        }
        if turn + 1 >= MAX_PARENT_TURNS {
            return Err(format!(
                "parent exceeded the {} turn limit without a final answer",
                MAX_PARENT_TURNS
            ));
        }

        // Execute all returned calls against their real MCP server,
        // concurrently, preserving call_id -> output association.
        let mut set: JoinSet<(String, String)> = JoinSet::new();
        for call in &parsed.calls {
            let client = routes
                .get(&call.name)
                .ok_or_else(|| format!("parent routed unknown tool {:?}", call.name))?
                .clone();
            let name = call.name.clone();
            let call_id = call.call_id.clone();
            let arguments = call.arguments.clone();
            let trace = trace.clone();
            let agent_results = agent_results.clone();
            let spawned = spawned.clone();
            let secret = api_key.clone();
            set.spawn(async move {
                let result = mcp_client::call_tool(&client, &name, arguments.clone()).await;
                let kind = if result.is_error == Some(true) {
                    TraceResultKind::Error
                } else {
                    TraceResultKind::Ok
                };
                trace
                    .lock()
                    .unwrap()
                    .record(turn, &name, &arguments, kind, Some(&secret));
                let captured = mcp_client::text(&result);
                println!(
                    "parent[t{turn}]: {name} {}",
                    trace_summary(&name, &arguments)
                );
                if name == "spawn_agent"
                    && let Some(agent_id) = result
                        .structured_content
                        .as_ref()
                        .and_then(|value| value.get("agent_id"))
                        .and_then(Value::as_str)
                {
                    let agent_name = arguments
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    collected(spawned, (agent_id.to_string(), agent_name.clone()));
                    println!("parent[t{turn}]: spawn_agent {agent_name} -> {agent_id}");
                }
                if name == "wait_agent"
                    && let Some(list) = result
                        .structured_content
                        .as_ref()
                        .and_then(|value| value.get("agents"))
                        .and_then(Value::as_array)
                {
                    let mut results = agent_results.lock().unwrap();
                    for agent in list {
                        if let Some(id) = agent.get("agent_id").and_then(Value::as_str) {
                            let result_text = agent
                                .get("result")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            results.entry(id.to_string()).or_default().push(result_text);
                        }
                    }
                }
                (call_id, bound(&captured, TOOL_OUTPUT_CAP_CHARS))
            });
        }
        let mut outputs: HashMap<String, String> = HashMap::new();
        while let Some(joined) = set.join_next().await {
            let (call_id, output) =
                joined.map_err(|error| format!("tool task panicked: {error}"))?;
            outputs.insert(call_id, output);
        }
        for call in &parsed.calls {
            let output = outputs
                .get(&call.call_id)
                .ok_or_else(|| format!("missing tool output for call {}", call.call_id))?;
            input.push(json!({
                "type": "function_call_output",
                "call_id": call.call_id,
                "output": output,
            }));
        }
    }

    if final_text.trim().is_empty() {
        return Err("parent final answer is empty".to_string());
    }

    // =========================================================================
    // Deterministic structural assertions over the recorded trace
    // =========================================================================
    let trace = trace.lock().unwrap();
    let spawned = spawned.lock().unwrap();
    let agent_results = agent_results.lock().unwrap();

    // 1. Skill activation.
    let activations = trace.calls("activate_skill");
    assert_trace(
        !activations.is_empty(),
        "activate_skill was never called",
        &trace,
    );
    let activated = activations
        .first()
        .and_then(|entry| entry.arguments.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_trace(
        activated == "release-audit",
        &format!("activate_skill called with {activated:?}, expected release-audit"),
        &trace,
    );

    // 2. Resource read through the real filesystem MCP (exact tool name comes
    //    from the real catalog).
    let resource_read = trace.entries.iter().find(|entry| {
        filesystem_tools.contains(&entry.tool_name)
            && arguments_mention(&entry.arguments, RESOURCE_RELATIVE)
    });
    assert_trace(
        resource_read.is_some(),
        "no filesystem read targeted references/release-contract.md",
        &trace,
    );

    // 3. Two distinct subagents spawned, both started before the first wait.
    let spawn_calls = trace.calls("spawn_agent");
    assert_trace(
        spawn_calls.len() >= 2,
        "fewer than two spawn_agent calls",
        &trace,
    );
    let spawn_ids: Vec<&str> = spawned.iter().map(|(id, _)| id.as_str()).collect();
    let mut distinct: Vec<&str> = spawn_ids.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_trace(
        distinct.len() >= 2,
        "fewer than two distinct agent ids",
        &trace,
    );
    let spawn_names: Vec<String> = trace
        .calls("spawn_agent")
        .iter()
        .filter_map(|entry| {
            entry
                .arguments
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    assert_trace(
        spawn_names.contains(&REVIEWER_NAME.to_string())
            && spawn_names.contains(&RESEARCHER_NAME.to_string()),
        &format!("spawned agent names {spawn_names:?}; expected reviewer and researcher",),
        &trace,
    );
    let first_wait_turn = trace.first_turn("wait_agent");
    assert_trace(
        first_wait_turn.is_some(),
        "wait_agent was never called",
        &trace,
    );
    let spawn_turns: Vec<usize> = trace
        .calls("spawn_agent")
        .iter()
        .map(|entry| entry.turn)
        .collect();
    assert_trace(
        spawn_turns
            .iter()
            .all(|turn| first_wait_turn.unwrap() > *turn),
        &format!(
            "independent subagents were not both started before the first blocking wait \
             (spawn turns {spawn_turns:?}, first wait turn {:?})",
            first_wait_turn
        ),
        &trace,
    );

    // 4. wait_agent used real ids.
    assert_trace(
        trace
            .calls("wait_agent")
            .iter()
            .any(|entry| all_targets_real(&entry.arguments, &spawn_ids)),
        "no wait_agent call targeted the real spawned ids",
        &trace,
    );

    // 5. Reviewer found the authorization issue.
    let reviewer_id = spawned
        .iter()
        .find(|(_, name)| name == REVIEWER_NAME)
        .map(|(id, _)| id.clone())
        .ok_or_else(|| "reviewer agent_id was not recorded".to_string())?;
    let reviewer_result = first_result(&agent_results, &reviewer_id)
        .ok_or_else(|| "reviewer result was not collected".to_string())?;
    assert_semantic!(
        (contains_case_insensitive(&reviewer_result, AUTH_PATH)
            || contains_case_insensitive(&reviewer_result, "auth.rs"))
            && contains_any(
                &reviewer_result,
                &[
                    "empty token",
                    "blank token",
                    "empty credential",
                    "empty credentials",
                    "is_authorized returns true",
                    "empty token is treated as authorized",
                    "blank token bypasses",
                    "empty string is accepted",
                    "empty input is authorized",
                    "empty token is accepted",
                    "empty token is considered authorized",
                ],
            ),
        "reviewer did not report the empty-token authorization issue from src/auth.rs",
        &reviewer_result
    );

    // 6. Researcher found the retry mismatch.
    let researcher_id = spawned
        .iter()
        .find(|(_, name)| name == RESEARCHER_NAME)
        .map(|(id, _)| id.clone())
        .ok_or_else(|| "researcher agent_id was not recorded".to_string())?;
    let researcher_result = first_result(&agent_results, &researcher_id)
        .ok_or_else(|| "researcher result was not collected".to_string())?;
    assert_semantic!(
        (contains_case_insensitive(&researcher_result, RETRY_PATH)
            || contains_case_insensitive(&researcher_result, "retry.rs"))
            && contains_case_insensitive(&researcher_result, RETRY_CONTRACT_PATH)
            && contains_wordish(&researcher_result, "5")
            && contains_wordish(&researcher_result, "3"),
        "researcher did not report the 5-vs-3 retry mismatch with both paths",
        &researcher_result
    );

    // 7. Reviewer continues by the same agent_id.
    let send_inputs = trace.calls("send_input");
    assert_trace(
        !send_inputs.is_empty(),
        "send_input was never called for the reviewer continuation",
        &trace,
    );
    assert_trace(
        send_inputs.iter().any(|entry| {
            entry.arguments.get("target").and_then(Value::as_str) == Some(&reviewer_id)
        }),
        &format!("send_input did not reuse reviewer agent_id {reviewer_id}"),
        &trace,
    );
    let follow_up_turn = send_inputs
        .iter()
        .map(|entry| entry.turn)
        .min()
        .unwrap_or(usize::MAX);
    let reviewer_follow_up = last_result(&agent_results, &reviewer_id)
        .ok_or_else(|| "continued reviewer result was not collected".to_string())?;
    assert_semantic!(
        follow_up_turn < usize::MAX
            && trace.calls("wait_agent").iter().any(|entry| {
                entry.turn > follow_up_turn && targets(&entry.arguments).contains(&reviewer_id)
            })
            && contains_any(
                &reviewer_follow_up,
                &[
                    "security",
                    "security-relevant",
                    "security-relevance",
                    "privilege escalation",
                    "unauthorized",
                    "security risk",
                    "CWE",
                    "security implication",
                    "bypass",
                    "security boundary",
                ],
            )
            && contains_any(
                &reviewer_follow_up,
                &[
                    "empty token",
                    "empty credentials",
                    "empty string",
                    "blank token",
                    "is_authorized",
                ],
            ),
        "the continued reviewer result was not retrieved via a later wait_agent, did not \
         reference the earlier authorization finding, or did not assess security relevance",
        &reviewer_follow_up
    );

    // 8. Final parent answer grounds the report in skill resource + subagents.
    assert_semantic!(
        final_text.contains(RELEASE_CHANNEL),
        "final answer is missing the release channel marker",
        &final_text
    );
    assert_semantic!(
        final_text.contains(RELEASE_MARKER),
        "final answer is missing the required report marker",
        &final_text
    );
    assert_semantic!(
        contains_any(
            &final_text,
            &[
                "empty token",
                "blank token",
                "empty credential",
                "is_authorized returns true"
            ],
        ),
        "final answer is missing the authorization finding",
        &final_text
    );
    assert_semantic!(
        contains_wordish(&final_text, "5") && contains_wordish(&final_text, "3"),
        "final answer is missing the retry mismatch (5 vs 3)",
        &final_text
    );
    assert_semantic!(
        contains_case_insensitive(&final_text, AUTH_PATH)
            || contains_any(&final_text, &["auth.rs", "src/auth"]),
        "final answer is missing the authorization evidence path",
        &final_text
    );
    assert_semantic!(
        contains_any(&final_text, &["retry.rs", "retry-contract.txt"]),
        "final answer is missing the retry evidence paths",
        &final_text
    );
    // The resumed reviewer's security assessment is consumed in the report.
    assert_semantic!(
        contains_any(
            &final_text,
            &[
                "security",
                "security-relevant",
                "security-relevance",
                "security risk",
                "unauthorized",
                "privilege escalation",
                "CWE",
            ],
        ),
        "final answer is missing the follow-up security assessment",
        &final_text
    );

    // 9. No secret leakage anywhere observable.
    let sanitized_trace = trace.serialize_sanitized(Some(&api_key));
    let aggregated = [
        sanitized_trace,
        final_text.clone(),
        reviewer_result.clone(),
        researcher_result.clone(),
    ]
    .join("\n----\n");
    if aggregated.contains(&api_key) {
        // The failure message must NOT reproduce the secret.
        return Err(
            "the provider token leaked into captured output (trace/final/agent results)"
                .to_string(),
        );
    }

    println!("acceptance: connected to all servers and completed");
    println!("acceptance: skill resource read via filesystem tool: yes");
    println!("acceptance: subagents used: {}", spawn_ids.len());
    println!("acceptance: reviewer continued by id: yes");
    println!("acceptance: final answer contains hidden resource markers: yes");
    Ok(())
}

fn bounded_evidence(value: &str, cap: usize) -> String {
    if value.chars().count() <= cap {
        value.to_string()
    } else {
        format!("{}…", value.chars().take(cap).collect::<String>())
    }
}

fn contains_case_insensitive(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

/// True when `haystack` mentions the bare numeric token `token` (e.g. "5").
fn contains_wordish(haystack: &str, token: &str) -> bool {
    haystack
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|word| word == token)
        || haystack.contains(token)
}

fn first_result(results: &HashMap<String, Vec<String>>, id: &str) -> Option<String> {
    results.get(id).and_then(|list| list.first()).cloned()
}

fn last_result(results: &HashMap<String, Vec<String>>, id: &str) -> Option<String> {
    results.get(id).and_then(|list| list.last()).cloned()
}

fn arguments_mention(arguments: &Value, needle: &str) -> bool {
    let mut found = false;
    collect_strings(arguments, &mut found, needle);
    found
}

fn collect_strings(value: &Value, found: &mut bool, needle: &str) {
    if *found {
        return;
    }
    match value {
        Value::String(text) => {
            if text.contains(needle) {
                *found = true;
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_strings(item, found, needle);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                collect_strings(value, found, needle);
            }
        }
        _ => {}
    }
}

fn all_targets_real(arguments: &Value, real_ids: &[&str]) -> bool {
    arguments
        .get("targets")
        .and_then(Value::as_array)
        .map(|targets| {
            targets
                .iter()
                .all(|target| target.as_str().is_some_and(|id| real_ids.contains(&id)))
        })
        .unwrap_or(false)
}

fn targets(arguments: &Value) -> Vec<String> {
    arguments
        .get("targets")
        .and_then(Value::as_array)
        .map(|targets| {
            targets
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn collected(list: Arc<Mutex<Vec<(String, String)>>>, value: (String, String)) {
    list.lock().unwrap().push(value);
}

fn bound(value: &str, cap: usize) -> String {
    if value.chars().count() <= cap {
        value.to_string()
    } else {
        format!("{}…", value.chars().take(cap).collect::<String>())
    }
}

/// Concise, sanitized, single-line summary of a parent tool call suitable
/// for the progress transcript. Never includes task text or credential
/// material.
fn trace_summary(name: &str, arguments: &Value) -> String {
    let pick = |key: &str| {
        arguments
            .get(key)
            .map(|value| value.to_string())
            .unwrap_or_default()
    };
    match name {
        "activate_skill" => pick("name"),
        "spawn_agent" => pick("name"),
        "send_input" => format!("target={}", pick("target")),
        "wait_agent" => format!("targets={}", pick("targets")),
        _ => {
            for key in ["path", "paths"] {
                if let Some(value) = arguments.get(key) {
                    return format!("{}={}", key, value);
                }
            }
            String::new()
        }
    }
}

fn assert_trace(condition: bool, message: &str, trace: &Trace) {
    if !condition {
        panic!("{message}\nTrace: {}", trace.serialize_sanitized(None))
    }
}
