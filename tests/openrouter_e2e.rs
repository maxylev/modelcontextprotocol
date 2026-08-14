//! OpenRouter end-to-end acceptance suite (ignored / gated).
//!
//! This binary contains:
//!
//! 1. `openrouter_e2e_acceptance` — `#[ignore]`d real-network test. It spawns
//!    every real MCP server in disposable environments, discovers them via
//!    the modern rmcp Discover lifecycle, derives every OpenRouter function
//!    schema from the runtime `Tool` definitions, runs one forced
//!    assistant → tool → assistant roundtrip per case, and validates the
//!    fetch prompt and memory resource through real bounded OpenRouter
//!    requests. It fails clearly when `OPENROUTER_API_KEY` is absent and is
//!    otherwise skipped by ordinary `cargo test` runs.
//! 2. `e2e_helper_command` — deterministic helper used by the shell server
//!    cases (argv/cwd/exit/stdout/stderr/timeout/truncation). Harmless
//!    no-op when run without the marker arguments.
//! 3. Offline unit tests for the schema normalizer/validator (in
//!    `openrouter/schema.rs`), which run under ordinary `cargo test`.

mod openrouter;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use openrouter::cases::{Case, CaseCtx, ServerId, assert_coverage, cases_for};
use openrouter::harness::{
    DEFAULT_MODEL, ENV_API_KEY, ENV_MODEL, FetchFixture, FetchFixtureMode, FsFixture, McpExecutor,
    MemFixture, OpenRouterClient, SUITE_BUDGET, ShellFixture, discover_and_list, fetch_prompt_text,
    read_resource_text, run_case,
};
use openrouter::report::Metrics;
use serde_json::Value;
use tokio::sync::Semaphore;

/// Marker argument that switches this test binary into helper mode for the
/// shell server cases (mirrors the pattern used by `tests/shell_server.rs`).
const HELPER_MARKER: &str = "e2e_helper_command";

/// The test binary doubles as a deterministic command helper: when spawned
/// with `--exact e2e_helper_command <mode> ...`, libtest runs only this
/// test, which performs the requested action and exits.
#[test]
fn e2e_helper_command() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 || args.get(3).map(String::as_str) != Some(HELPER_MARKER) {
        return;
    }
    use std::io::Write;
    match args[4].as_str() {
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
        _ => {}
    }
}

/// The ignored acceptance suite: real OpenRouter + real MCP servers.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "real-network acceptance suite; requires OPENROUTER_API_KEY and spends tokens"]
async fn openrouter_e2e_acceptance() {
    let metrics = Metrics::new();

    let api_key = match std::env::var(ENV_API_KEY) {
        Ok(key) if !key.trim().is_empty() => key,
        _ => panic!(
            "openrouter_e2e_acceptance requires the {ENV_API_KEY} environment variable. \
             This test is ignored by default; set the key in your environment and run: \
             env -u {ENV_MODEL} cargo test --test openrouter_e2e -- --ignored --nocapture \
             --test-threads=1"
        ),
    };

    // Model resolution: OPENROUTER_MODEL override is accepted for
    // diagnostics, but acceptance defaults to the exact required alias.
    let (model, model_origin) = match std::env::var(ENV_MODEL) {
        Ok(override_model) if !override_model.trim().is_empty() => {
            (override_model, format!("{ENV_MODEL} override"))
        }
        _ => (DEFAULT_MODEL.to_string(), "default alias".to_string()),
    };
    println!("OpenRouter e2e acceptance starting (model origin: {model_origin})");
    println!("requested model: {model}");
    if model != DEFAULT_MODEL {
        println!(
            "NOTE: {ENV_MODEL} is set; acceptance default would be the exact alias \
             {DEFAULT_MODEL:?}"
        );
    }

    let client = Arc::new(OpenRouterClient::new(
        api_key,
        model.clone(),
        metrics.clone(),
    ));

    // Whole-suite budget guard. Failure strings are collected loss-proof in
    // the shared metrics, so an abort still reports everything so far.
    let suite = async {
        let (fs_failures, mem_failures, fetch_failures, shell_failures) = tokio::join!(
            run_filesystem_phase(client.clone(), &metrics),
            run_memory_phase(client.clone(), &metrics),
            run_fetch_phase(client.clone(), &metrics),
            run_shell_phase(client.clone(), &metrics),
        );
        let mut failures = Vec::new();
        failures.extend(fs_failures);
        failures.extend(mem_failures);
        failures.extend(fetch_failures);
        failures.extend(shell_failures);
        failures.extend(run_prompt_and_resource_consumption(client.clone()).await);
        for failure in &failures {
            metrics.failure(failure.clone());
        }
        failures
    };
    match tokio::time::timeout(SUITE_BUDGET, suite).await {
        Ok(_) => {}
        Err(_) => {
            println!(
                "suite exceeded the {}s budget; aborting",
                SUITE_BUDGET.as_secs()
            );
            metrics.failure("suite exceeded the time budget".to_string());
        }
    };

    openrouter::report::print_summary(&metrics, &model);
    openrouter::report::panic_on_failures(&metrics);
}

// ---------------------------------------------------------------------------
// Group execution
// ---------------------------------------------------------------------------

/// Shared context for group execution.
struct GroupRunner {
    client: Arc<OpenRouterClient>,
    metrics: Arc<Metrics>,
    executor: Arc<McpExecutor>,
    tools: Arc<Vec<rmcp::model::Tool>>,
    ctx: CaseCtx,
    fetch_bases: Arc<HashMap<&'static str, String>>,
    concurrency: usize,
}

impl GroupRunner {
    /// Run one execution group: all cases are independent, so they run
    /// concurrently bounded by `concurrency`. Returns the failures.
    async fn run(&self, group: Vec<Case>) -> Vec<String> {
        let semaphore = Arc::new(Semaphore::new(self.concurrency));
        let mut tasks = tokio::task::JoinSet::new();
        for case in group {
            let client = Arc::clone(&self.client);
            let metrics = Arc::clone(&self.metrics);
            let executor = Arc::clone(&self.executor);
            let tools = Arc::clone(&self.tools);
            let ctx = self.ctx.clone();
            let semaphore = Arc::clone(&semaphore);
            let fetch_bases = Arc::clone(&self.fetch_bases);
            tasks.spawn(async move {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .expect("semaphore not closed");
                let base = fetch_bases.get(case.id).map(String::as_str);
                run_case(&client, &case, &ctx, &tools, metrics, &executor, base).await
            });
        }
        let mut failures = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(Ok(())) => {}
                Ok(Err(e)) => failures.push(e),
                Err(e) => failures.push(format!("case task panicked: {e}")),
            }
        }
        failures
    }
}

/// Split the catalog cases of one server into ordered execution groups.
fn groups(cases: &[Case]) -> Vec<Vec<&Case>> {
    let max_group = cases.iter().map(|c| c.group).max().unwrap_or(0);
    (0..=max_group)
        .map(|g| cases.iter().filter(|c| c.group == g).collect())
        .filter(|group: &Vec<&Case>| !group.is_empty())
        .collect()
}

fn base_ctx(
    fs_root: std::path::PathBuf,
    mem_file: std::path::PathBuf,
    shell_root: std::path::PathBuf,
    shell_work: std::path::PathBuf,
) -> CaseCtx {
    CaseCtx {
        fs_root,
        mem_file,
        bin: env!("CARGO_BIN_EXE_modelcontextprotocol"),
        helper: std::env::current_exe()
            .expect("test binary path")
            .to_string_lossy()
            .into_owned(),
        shell_root,
        shell_work,
    }
}

// ---------------------------------------------------------------------------
// Phases
// ---------------------------------------------------------------------------

async fn run_filesystem_phase(
    client: Arc<OpenRouterClient>,
    metrics: &Arc<Metrics>,
) -> Vec<String> {
    let mut failures = Vec::new();
    let fixture = FsFixture::new();
    let cases = cases_for(ServerId::Filesystem);
    let args: Vec<String> = vec![
        "filesystem".to_string(),
        fixture.root.to_str().expect("utf8 path").to_string(),
    ];
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let (mcp, tools, prompts, resources) =
        match discover_and_list(&argv, "mcp-filesystem", true, false, false).await {
            Ok(result) => result,
            Err(e) => {
                failures.push(format!("filesystem phase: {e}"));
                return failures;
            }
        };
    if !prompts.is_empty() || !resources.is_empty() {
        failures.push(format!(
            "filesystem phase: unexpected prompts/resources: {prompts:?} {resources:?}"
        ));
    }

    let runtime = runtime_inventory(&tools);
    let violations = assert_coverage(ServerId::Filesystem, &runtime, &cases);
    if !violations.is_empty() {
        failures.extend(
            violations
                .into_iter()
                .map(|v| format!("filesystem coverage: {v}")),
        );
        return failures;
    }

    let ctx = base_ctx(
        fixture.root.clone(),
        std::path::PathBuf::new(),
        std::path::PathBuf::new(),
        std::path::PathBuf::new(),
    );
    let executor = Arc::new(McpExecutor { client: mcp });
    let tools = Arc::new(tools);
    let runner = GroupRunner {
        client: Arc::clone(&client),
        metrics: Arc::clone(metrics),
        executor: Arc::clone(&executor),
        tools: Arc::clone(&tools),
        ctx,
        fetch_bases: Arc::new(HashMap::new()),
        concurrency: 4,
    };
    for group in groups(&cases) {
        failures.extend(runner.run(group.into_iter().cloned().collect()).await);
    }
    failures
}

async fn run_memory_phase(client: Arc<OpenRouterClient>, metrics: &Arc<Metrics>) -> Vec<String> {
    let mut failures = Vec::new();
    let fixture = MemFixture::new();
    let cases = cases_for(ServerId::Memory);
    let args: Vec<String> = vec![
        "memory".to_string(),
        "--memory-file".to_string(),
        fixture.file.to_str().expect("utf8 path").to_string(),
    ];
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let (mcp, tools, prompts, resources) =
        match discover_and_list(&argv, "mcp-memory", true, false, true).await {
            Ok(result) => result,
            Err(e) => {
                failures.push(format!("memory phase: {e}"));
                return failures;
            }
        };
    if !prompts.is_empty() {
        failures.push(format!("memory phase: unexpected prompts: {prompts:?}"));
    }
    if resources != ["memory://knowledge-graph"] {
        failures.push(format!("memory phase: unexpected resources: {resources:?}"));
    }

    let runtime = runtime_inventory(&tools);
    let violations = assert_coverage(ServerId::Memory, &runtime, &cases);
    if !violations.is_empty() {
        failures.extend(
            violations
                .into_iter()
                .map(|v| format!("memory coverage: {v}")),
        );
        return failures;
    }

    let ctx = base_ctx(
        std::path::PathBuf::new(),
        fixture.file.clone(),
        std::path::PathBuf::new(),
        std::path::PathBuf::new(),
    );
    let tools = Arc::new(tools);
    let mut executor = Arc::new(McpExecutor { client: mcp });
    for group in groups(&cases) {
        if group.iter().any(|c| c.respawn) {
            println!("memory phase: respawning server for case {}", group[0].id);
            match discover_and_list(&argv, "mcp-memory", true, false, true).await {
                Ok((new_mcp, new_tools, _, _)) => {
                    let _ = &new_tools;
                    executor = Arc::new(McpExecutor { client: new_mcp });
                }
                Err(e) => {
                    failures.push(format!("memory respawn: {e}"));
                    return failures;
                }
            }
        }
        let runner = GroupRunner {
            client: Arc::clone(&client),
            metrics: Arc::clone(metrics),
            executor: Arc::clone(&executor),
            tools: Arc::clone(&tools),
            ctx: ctx.clone(),
            fetch_bases: Arc::new(HashMap::new()),
            concurrency: 4,
        };
        failures.extend(runner.run(group.into_iter().cloned().collect()).await);
    }
    failures
}

async fn run_fetch_phase(client: Arc<OpenRouterClient>, metrics: &Arc<Metrics>) -> Vec<String> {
    let mut failures = Vec::new();
    let cases = cases_for(ServerId::Fetch);
    let argv = ["fetch"];
    let (mcp, tools, prompts, resources) =
        match discover_and_list(&argv, "mcp-fetch", true, true, false).await {
            Ok(result) => result,
            Err(e) => {
                failures.push(format!("fetch phase: {e}"));
                return failures;
            }
        };
    if prompts != ["fetch"] {
        failures.push(format!("fetch phase: unexpected prompts: {prompts:?}"));
    }
    if !resources.is_empty() {
        failures.push(format!("fetch phase: unexpected resources: {resources:?}"));
    }

    let runtime = runtime_inventory(&tools);
    let violations = assert_coverage(ServerId::Fetch, &runtime, &cases);
    if !violations.is_empty() {
        failures.extend(
            violations
                .into_iter()
                .map(|v| format!("fetch coverage: {v}")),
        );
        return failures;
    }

    // Every fetch case gets its own deterministic local fixture (robots mode
    // included), so no public internet is needed for correctness. Fixtures
    // live for the whole phase; base URLs are handed to the group runner.
    let mut fixtures = Vec::new();
    let mut bases: HashMap<&'static str, String> = HashMap::new();
    for case in &cases {
        let fixture = FetchFixture::start(match case.fetch_mode {
            FetchFixtureMode::Plain => FetchFixtureMode::Plain,
            FetchFixtureMode::AllowAll => FetchFixtureMode::AllowAll,
            FetchFixtureMode::DisallowAll => FetchFixtureMode::DisallowAll,
        });
        bases.insert(case.id, fixture.base_url.clone());
        fixtures.push(fixture);
    }

    let ctx = base_ctx(
        std::path::PathBuf::new(),
        std::path::PathBuf::new(),
        std::path::PathBuf::new(),
        std::path::PathBuf::new(),
    );
    let executor = Arc::new(McpExecutor { client: mcp });
    let tools = Arc::new(tools);
    let runner = GroupRunner {
        client: Arc::clone(&client),
        metrics: Arc::clone(metrics),
        executor: Arc::clone(&executor),
        tools: Arc::clone(&tools),
        ctx,
        fetch_bases: Arc::new(bases),
        concurrency: 3,
    };
    for group in groups(&cases) {
        failures.extend(runner.run(group.into_iter().cloned().collect()).await);
    }
    let _ = fixtures;
    failures
}

async fn run_shell_phase(client: Arc<OpenRouterClient>, metrics: &Arc<Metrics>) -> Vec<String> {
    let mut failures = Vec::new();
    let fixture = ShellFixture::new();
    let cases = cases_for(ServerId::Shell);
    let args: Vec<String> = vec![
        "shell".to_string(),
        fixture.root.to_str().expect("utf8 path").to_string(),
    ];
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let (mcp, tools, prompts, resources) =
        match discover_and_list(&argv, "mcp-shell", true, false, false).await {
            Ok(result) => result,
            Err(e) => {
                failures.push(format!("shell phase: {e}"));
                return failures;
            }
        };
    if !prompts.is_empty() || !resources.is_empty() {
        failures.push(format!(
            "shell phase: unexpected prompts/resources: {prompts:?} {resources:?}"
        ));
    }

    let runtime = runtime_inventory(&tools);
    let violations = assert_coverage(ServerId::Shell, &runtime, &cases);
    if !violations.is_empty() {
        failures.extend(
            violations
                .into_iter()
                .map(|v| format!("shell coverage: {v}")),
        );
        return failures;
    }

    let ctx = base_ctx(
        std::path::PathBuf::new(),
        std::path::PathBuf::new(),
        fixture.root.clone(),
        fixture.work.clone(),
    );
    let executor = Arc::new(McpExecutor { client: mcp });
    let tools = Arc::new(tools);
    let runner = GroupRunner {
        client: Arc::clone(&client),
        metrics: Arc::clone(metrics),
        executor: Arc::clone(&executor),
        tools: Arc::clone(&tools),
        ctx,
        fetch_bases: Arc::new(HashMap::new()),
        concurrency: 2,
    };
    for group in groups(&cases) {
        failures.extend(runner.run(group.into_iter().cloned().collect()).await);
    }
    failures
}

/// Fetch prompt + memory resource: MCP retrieval, then one bounded real
/// OpenRouter request each whose user content is fixture-backed.
async fn run_prompt_and_resource_consumption(client: Arc<OpenRouterClient>) -> Vec<String> {
    let mut failures = Vec::new();
    // Fetch prompt (manual user agent; robots not consulted).
    let (mcp, _, _, _) = match discover_and_list(&["fetch"], "mcp-fetch", true, true, false).await {
        Ok(result) => result,
        Err(e) => {
            failures.push(format!("prompt phase: {e}"));
            return failures;
        }
    };
    let fixture = FetchFixture::start(FetchFixtureMode::Plain);
    match fetch_prompt_text(&mcp, &fixture.url("/page")).await {
        Ok((text, description)) => {
            if text.is_empty() {
                failures.push("prompt phase: fetch prompt returned empty content".to_string());
                return failures;
            }
            if description.as_deref().unwrap_or_default().is_empty() {
                failures.push("prompt phase: fetch prompt returned no description".to_string());
            }
            let capped: String = text.chars().take(4000).collect();
            println!(
                "prompt consumption: fetch prompt yielded {} chars of markdown",
                text.chars().count()
            );
            match client
                .consume(format!("Fetched page content:\n{capped}"), "fetch-prompt")
                .await
            {
                Ok((_content, request_id, model, usage, elapsed)) => {
                    println!(
                        "prompt consumption request ok: id={request_id} model={model} \
                         elapsed={elapsed}ms usage in={} out={}",
                        usage.prompt_tokens.unwrap_or(0),
                        usage.completion_tokens.unwrap_or(0)
                    );
                }
                Err(e) => failures.push(e),
            }
        }
        Err(e) => failures.push(e),
    }

    // Memory resource: read through MCP, then consume in a real request.
    let fixture_mem = MemFixture::new();
    let args: Vec<String> = vec![
        "memory".to_string(),
        "--memory-file".to_string(),
        fixture_mem.file.to_str().expect("utf8 path").to_string(),
    ];
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let (mcp_mem, _, _, _) = match discover_and_list(&argv, "mcp-memory", true, false, true).await {
        Ok(result) => result,
        Err(e) => {
            failures.push(format!("resource phase: {e}"));
            return failures;
        }
    };
    // Seed the graph so the resource is fixture-backed.
    use rmcp::model::CallToolRequestParams;
    let seed = mcp_mem
        .call_tool(
            CallToolRequestParams::new("create_entities".to_string()).with_arguments(
                serde_json::json!({
                    "entities": [{
                        "name": "graph-fixture",
                        "entityType": "node",
                        "observations": ["resource-backed"]
                    }]
                })
                .as_object()
                .expect("object")
                .clone(),
            ),
        )
        .await;
    if let Err(e) = seed {
        failures.push(format!("resource phase: seeding graph failed: {e}"));
        return failures;
    }
    match read_resource_text(&mcp_mem, "memory://knowledge-graph").await {
        Ok(text) => {
            if !text.contains("graph-fixture") {
                failures.push("resource phase: resource content missing seeded entity".to_string());
                return failures;
            }
            let capped: String = text.chars().take(4000).collect();
            println!(
                "resource consumption: memory resource yielded {} chars of JSON",
                text.chars().count()
            );
            match client
                .consume(
                    format!("Knowledge graph resource:\n{capped}"),
                    "memory-resource",
                )
                .await
            {
                Ok((_content, request_id, model, usage, elapsed)) => {
                    println!(
                        "resource consumption request ok: id={request_id} model={model} \
                         elapsed={elapsed}ms usage in={} out={}",
                        usage.prompt_tokens.unwrap_or(0),
                        usage.completion_tokens.unwrap_or(0)
                    );
                }
                Err(e) => failures.push(e),
            }
        }
        Err(e) => failures.push(e),
    }
    failures
}

/// Extract (tool name, parameter names, required parameter names) from the
/// runtime tool inventory.
fn runtime_inventory(tools: &[rmcp::model::Tool]) -> Vec<(String, Vec<String>, Vec<String>)> {
    tools
        .iter()
        .map(|tool| {
            let schema = tool.schema_as_json_value();
            let properties: Vec<String> = schema
                .get("properties")
                .and_then(Value::as_object)
                .map(|props| props.keys().cloned().collect())
                .unwrap_or_default();
            let required: Vec<String> = schema
                .get("required")
                .and_then(Value::as_array)
                .map(|req| {
                    req.iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            (tool.name.to_string(), properties, required)
        })
        .collect()
}
