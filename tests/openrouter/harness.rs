//! Real-network harness: OpenRouter client with strict bounds and retry
//! policy, disposable MCP server spawning with modern Discover lifecycle,
//! deterministic local fixtures, and the forced tool-call roundtrip driver.
//!
//! Nothing in this module is executed by an ordinary `cargo test` run: the
//! only entry point is `openrouter_e2e::openrouter_e2e_acceptance`, which is
//! `#[ignore]`d and requires `OPENROUTER_API_KEY`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rmcp::{
    RoleClient,
    model::{
        CallToolRequestParams, CallToolResult, ContentBlock, GetPromptRequestParams,
        ProtocolVersion, ReadResourceRequestParams,
    },
    service::{ClientLifecycleMode, ClientServiceExt, RunningService},
    transport::TokioChildProcess,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::process::Command;

use super::cases::{Case, CaseCtx, Oracle};
use super::report::Metrics;
use super::schema::{Normalized, normalize_tool_schema};

// ---------------------------------------------------------------------------
// Constants and configuration
// ---------------------------------------------------------------------------

/// Exact required default model alias (OpenRouter `~` = latest of the named
/// model family). The response `model` field is the concrete resolved model.
pub const DEFAULT_MODEL: &str = "~deepseek/deepseek-v4-flash-latest";

/// Environment variables honored by the suite.
pub const ENV_API_KEY: &str = "OPENROUTER_API_KEY";
pub const ENV_MODEL: &str = "OPENROUTER_MODEL";

pub const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Per-request hard timeout (task requirement: <= 45s).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
/// Bounded response body (task requirement: <= 1 MiB).
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
/// No more than 2 attempts per request, and only for retryable conditions.
pub const MAX_ATTEMPTS: u32 = 2;

/// Modest token ceilings. The final-answer ceiling is 200: high enough that
/// a confirmation answer never hits the `length` finish reason (which
/// produced empty finals at 128). The tool-call ceiling is 512 so a verbose
/// provider can emit the forced tool call without truncating its arguments
/// mid-JSON (256 produced malformed, truncated arguments).
pub const MAX_TOKENS_TOOL_CALL: u32 = 512;
pub const MAX_TOKENS_FINAL: u32 = 200;

/// Bounds for the final assistant response assertion.
pub const FINAL_MIN_CHARS: usize = 1;
pub const FINAL_MAX_CHARS: usize = 2048;

/// Overall suite budget; a guard so the acceptance run stays bounded
/// (target total suite time <= 15 minutes).
pub const SUITE_BUDGET: Duration = Duration::from_secs(14 * 60 + 30);

/// MCP tool results fed back to the model are capped at this many characters.
pub const TOOL_RESULT_CAP: usize = 4000;

pub type Client = RunningService<RoleClient, ()>;

// ---------------------------------------------------------------------------
// OpenRouter client
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Usage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatResponse {
    id: Option<String>,
    model: Option<String>,
    choices: Option<Vec<Choice>>,
    usage: Option<Usage>,
    error: Option<ApiError>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiError {
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Choice {
    message: Message,
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Message {
    content: Option<String>,
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
struct ToolCall {
    id: String,
    function: Function,
}

#[derive(Debug, Clone, Deserialize)]
struct Function {
    name: String,
    arguments: String,
}

/// One chat message, serialized to the exact OpenAI-compatible wire shape.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: &'static str, // "user" | "assistant" | "tool"
    pub content: Option<String>,
    pub tool_calls: Option<Vec<Value>>,
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user",
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }
    pub fn assistant_tool_calls(tool_calls: Vec<Value>) -> Self {
        Self {
            role: "assistant",
            content: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }
    pub fn tool_result(tool_call_id: String, content: String) -> Self {
        Self {
            role: "tool",
            content: Some(content),
            tool_calls: None,
            tool_call_id: Some(tool_call_id),
        }
    }
    fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("role".to_string(), Value::String(self.role.to_string()));
        if let Some(content) = &self.content {
            map.insert("content".to_string(), Value::String(content.clone()));
        }
        if let Some(calls) = &self.tool_calls {
            map.insert("tool_calls".to_string(), Value::Array(calls.clone()));
        }
        if let Some(id) = &self.tool_call_id {
            map.insert("tool_call_id".to_string(), Value::String(id.clone()));
        }
        Value::Object(map)
    }
}

/// Outcome of a completed forced tool roundtrip.
pub struct Roundtrip {
    pub mcp_result: CallToolResult,
    pub request_ids: Vec<String>,
    pub actual_models: Vec<String>,
    pub usage: Usage,
    pub retries: u32,
}

pub struct OpenRouterClient {
    http: reqwest::Client,
    api_key: String,
    pub model: String,
    metrics: Arc<Metrics>,
}

impl OpenRouterClient {
    pub fn new(api_key: String, model: String, metrics: Arc<Metrics>) -> Self {
        // reqwest is built with rustls-no-provider; install the ring provider
        // like the fetch server does, otherwise TLS handshakes panic.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("build reqwest client");
        Self {
            http,
            api_key,
            model,
            metrics,
        }
    }

    /// One POST with the exact required OpenRouter call shape:
    /// `parallel_tool_calls: false`, `stream: false`, `temperature: 0`,
    /// tools as `{"type":"function","function":{...}}`.
    ///
    /// Retry policy: at most [`MAX_ATTEMPTS`] attempts, retrying only
    /// transport errors, 429s, and 5xx. All other failures (4xx, unparseable
    /// bodies, oversize bodies, schema/assertion problems) are returned as
    /// hard errors and never retried. The caller decides.
    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[Value]>,
        tool_choice: Option<Value>,
        max_tokens: u32,
    ) -> Result<ChatResponse, String> {
        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(self.model.clone()));
        body.insert(
            "messages".to_string(),
            Value::Array(messages.iter().map(ChatMessage::to_json).collect()),
        );
        if let Some(tools) = tools {
            body.insert("tools".to_string(), Value::Array(tools.to_vec()));
        }
        if let Some(choice) = tool_choice {
            body.insert("tool_choice".to_string(), choice);
        }
        body.insert("parallel_tool_calls".to_string(), Value::Bool(false));
        body.insert("stream".to_string(), Value::Bool(false));
        body.insert("temperature".to_string(), Value::Number(0.into()));
        body.insert("max_tokens".to_string(), Value::Number(max_tokens.into()));

        let mut last_error: Option<String>;
        let mut attempts = 0u32;
        loop {
            attempts += 1;
            self.metrics
                .counters
                .requests
                .fetch_add(1, Ordering::SeqCst);
            let request = self
                .http
                .post(ENDPOINT)
                .bearer_auth(&self.api_key)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(
                    serde_json::to_string(&Value::Object(body.clone()))
                        .map_err(|e| format!("serialize request body: {e}"))?,
                );
            let response = request.send().await;
            let response = match response {
                Ok(response) => response,
                Err(e) => {
                    last_error = Some(format!("transport error: {e}"));
                    if attempts < MAX_ATTEMPTS {
                        self.backoff(None).await;
                        continue;
                    }
                    return Err(last_error.unwrap_or_default());
                }
            };
            let status = response.status();
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let bytes = match read_bounded(response, MAX_BODY_BYTES).await {
                Ok(bytes) => bytes,
                Err(e) => {
                    last_error = Some(e);
                    if attempts < MAX_ATTEMPTS {
                        self.backoff(None).await;
                        continue;
                    }
                    return Err(last_error.unwrap_or_default());
                }
            };
            let parsed: Result<ChatResponse, _> = serde_json::from_slice(&bytes);
            let parsed = match parsed {
                Ok(parsed) => parsed,
                Err(e) => {
                    // Unparseable bodies are never retried: they indicate a
                    // contract problem, not a transient failure.
                    return Err(format!(
                        "unparseable response (status {status}, {} bytes): {e}",
                        bytes.len()
                    ));
                }
            };
            if status.is_success() {
                if let Some(api_error) = &parsed.error {
                    return Err(format!(
                        "API error in 200 response: {}",
                        api_error
                            .message
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string())
                    ));
                }
                return Ok(parsed);
            }
            let retryable = status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || status.is_server_error()
                || retry_after.is_some();
            let message = parsed
                .error
                .as_ref()
                .and_then(|e| e.message.clone())
                .unwrap_or_else(|| "no error detail".to_string());
            last_error = Some(format!("status {status}: {message}"));
            if retryable && attempts < MAX_ATTEMPTS {
                self.backoff(retry_after).await;
                continue;
            }
            return Err(last_error.unwrap_or_default());
        }
    }

    async fn backoff(&self, retry_after: Option<u64>) {
        // Cap any server-suggested wait so the suite stays bounded.
        let seconds = retry_after.unwrap_or(2).min(5);
        tokio::time::sleep(Duration::from_secs(seconds)).await;
        self.metrics.counters.retries.fetch_add(1, Ordering::SeqCst);
    }

    /// Run the full assistant -> tool -> assistant roundtrip for one forced
    /// case. The model must emit exactly one tool call whose arguments are
    /// schema-valid and equal to the intended ones (tolerating only
    /// schema-declared default padding); then the MCP tool executes, the
    /// result is returned with the matching `tool_call_id`, the tools are
    /// resent, and the final assistant response must be bounded and
    /// non-empty. Schema or assertion deviations are never retried.
    pub async fn forced_roundtrip(
        &self,
        case: &Case,
        function: &Value, // {"type":"function","function":{...}}
        normalized: &Normalized,
        intended: &Value,
        executor: &McpExecutor,
    ) -> Result<Roundtrip, String> {
        let retries_before = self.metrics.counters.retries.load(Ordering::SeqCst);

        let instruction = format!(
            "You are a deterministic test harness. When asked to call a tool, call it with \
             EXACTLY the arguments provided: do not modify, add, omit, or rename any \
             argument. If you cannot comply, say so instead of calling the tool.\n\n\
             Call the tool \"{name}\" now with exactly these arguments and nothing else: \
             {args}\n\nAfter the tool result is returned, you will answer in a second step.",
            name = case.tool,
            args = serde_json::to_string(intended).unwrap_or_default()
        );
        let messages = vec![ChatMessage::user(instruction)];
        let tool_choice = Value::Object(Map::from_iter([
            ("type".to_string(), Value::String("function".to_string())),
            (
                "function".to_string(),
                Value::Object(Map::from_iter([(
                    "name".to_string(),
                    Value::String(case.tool.to_string()),
                )])),
            ),
        ]));
        let tools = vec![function.clone()];

        // Request 1: forced tool call.
        let response1 = self
            .complete(
                &messages,
                Some(&tools),
                Some(tool_choice),
                MAX_TOKENS_TOOL_CALL,
            )
            .await
            .map_err(|e| format!("request 1 failed: {e}"))?;
        let request_id1 = response1.id.clone().unwrap_or_default();
        let model1 = response1.model.clone().unwrap_or_default();
        let choices = response1
            .choices
            .as_deref()
            .ok_or_else(|| "request 1: response has no choices".to_string())?;
        let choice = choices
            .first()
            .ok_or_else(|| "request 1: empty choices".to_string())?;
        let tool_calls = choice.message.tool_calls.as_deref().ok_or_else(|| {
            format!(
                "request 1: no tool_calls in assistant message (finish_reason {:?}); \
                     model refused or answered without calling the tool",
                choice.finish_reason
            )
        })?;
        if tool_calls.len() != 1 {
            return Err(format!(
                "request 1: expected exactly 1 tool call, got {}",
                tool_calls.len()
            ));
        }
        let call = &tool_calls[0];
        if call.function.name != case.tool {
            return Err(format!(
                "deviation: model called tool {:?} instead of {:?}",
                call.function.name, case.tool
            ));
        }
        let model_args: Value = serde_json::from_str(&call.function.arguments)
            .map_err(|e| format!("deviation: tool arguments are not valid JSON: {e}"))?;
        if !model_args.is_object() {
            return Err(format!(
                "deviation: tool arguments are not a JSON object: {}",
                describe_bounded(&model_args, 200)
            ));
        }

        // Block unsafe deviations before MCP execution: schema validation
        // plus case-level constraints (tolerating only schema-declared
        // defaults the model padded in).
        let padded = strip_default_padding(model_args.clone(), intended, normalized);
        if let Err(errors) = super::schema::validate(&padded, &normalized.schema) {
            return Err(format!(
                "deviation: model arguments fail schema validation: {}",
                errors.join("; ")
            ));
        }
        if padded != *intended {
            return Err(format!(
                "deviation: model arguments {} differ from intended {}",
                describe_bounded(&padded, 400),
                describe_bounded(intended, 400)
            ));
        }

        // Execute the MCP tool with the validated, exact arguments.
        let mcp_result = executor
            .call(case, &padded)
            .await
            .map_err(|e| format!("MCP transport error for {}: {e}", case.tool))?;

        // Request 2: return the tool result with the matching tool_call_id,
        // resend the tools, and force a plain final answer.
        let tool_message_text = mcp_result_to_text(&mcp_result, TOOL_RESULT_CAP);
        let mut messages = messages;
        messages.push(ChatMessage::assistant_tool_calls(vec![Value::Object(
            Map::from_iter([
                ("id".to_string(), Value::String(call.id.clone())),
                ("type".to_string(), Value::String("function".to_string())),
                (
                    "function".to_string(),
                    Value::Object(Map::from_iter([
                        (
                            "name".to_string(),
                            Value::String(call.function.name.clone()),
                        ),
                        (
                            "arguments".to_string(),
                            Value::String(call.function.arguments.clone()),
                        ),
                    ])),
                ),
            ]),
        )]));
        messages.push(ChatMessage::tool_result(call.id.clone(), tool_message_text));
        // A trailing user turn guarantees the model produces a non-empty
        // final answer instead of ending on the tool result.
        messages.push(ChatMessage::user(
            "The tool call is complete. Answer directly with one or two sentences \
             describing what the tool returned. Do not analyze, do not plan, do not call \
             any tool.",
        ));
        let response2 = self
            .complete(
                &messages,
                Some(&tools),
                Some(Value::String("none".to_string())),
                MAX_TOKENS_FINAL,
            )
            .await
            .map_err(|e| format!("request 2 failed: {e}"))?;
        let request_id2 = response2.id.clone().unwrap_or_default();
        let model2 = response2.model.clone().unwrap_or_default();

        let final_choice = response2
            .choices
            .as_deref()
            .and_then(|choices| choices.first())
            .ok_or_else(|| "request 2: no choices".to_string())?;
        let final_content = final_choice.message.content.clone().unwrap_or_default();
        let final_trimmed = final_content.trim();
        let final_len = final_trimmed.chars().count();
        if !(FINAL_MIN_CHARS..=FINAL_MAX_CHARS).contains(&final_len) {
            return Err(format!(
                "request 2: final assistant response length {final_len} outside the bounded \
                 range {FINAL_MIN_CHARS}..={FINAL_MAX_CHARS} chars \
                 (finish_reason {:?}, usage {:?})",
                final_choice.finish_reason, response2.usage
            ));
        }

        let usage = match (&response1.usage, &response2.usage) {
            (Some(a), Some(b)) => Usage {
                prompt_tokens: Some(a.prompt_tokens.unwrap_or(0) + b.prompt_tokens.unwrap_or(0)),
                completion_tokens: Some(
                    a.completion_tokens.unwrap_or(0) + b.completion_tokens.unwrap_or(0),
                ),
                total_tokens: Some(a.total_tokens.unwrap_or(0) + b.total_tokens.unwrap_or(0)),
            },
            (Some(a), None) => a.clone(),
            (None, Some(b)) => b.clone(),
            (None, None) => Usage::default(),
        };
        self.metrics
            .counters
            .tokens_in
            .fetch_add(usage.prompt_tokens.unwrap_or(0), Ordering::SeqCst);
        self.metrics
            .counters
            .tokens_out
            .fetch_add(usage.completion_tokens.unwrap_or(0), Ordering::SeqCst);

        Ok(Roundtrip {
            mcp_result,
            request_ids: vec![request_id1, request_id2],
            actual_models: vec![model1, model2],
            usage,
            retries: self.metrics.counters.retries.load(Ordering::SeqCst) - retries_before,
        })
    }

    /// A bounded single-turn completion without tools (used to consume the
    /// fetch prompt and the memory resource in real requests). Returns
    /// (final text, request id, actual model, usage, elapsed ms).
    pub async fn consume(
        &self,
        user_content: String,
        kind: &'static str,
    ) -> Result<(String, String, String, Usage, u64), String> {
        let started = Instant::now();
        let messages = vec![
            ChatMessage::user(
                "You are a deterministic test harness. Briefly confirm what the provided \
                 content says in at most 100 words."
                    .to_string(),
            ),
            ChatMessage::user(user_content),
        ];
        let response = self
            .complete(&messages, None, None, MAX_TOKENS_FINAL)
            .await
            .map_err(|e| format!("{kind} consumption request failed: {e}"))?;
        let request_id = response.id.clone().unwrap_or_default();
        let model = response.model.clone().unwrap_or_default();
        self.metrics
            .counters
            .consumption_requests
            .fetch_add(1, Ordering::SeqCst);
        let content = response
            .choices
            .as_deref()
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.message.content.clone())
            .unwrap_or_default();
        if content.trim().is_empty() {
            return Err(format!("{kind} consumption: final response is empty"));
        }
        let usage = response.usage.clone().unwrap_or_default();
        self.metrics
            .counters
            .tokens_in
            .fetch_add(usage.prompt_tokens.unwrap_or(0), Ordering::SeqCst);
        self.metrics
            .counters
            .tokens_out
            .fetch_add(usage.completion_tokens.unwrap_or(0), Ordering::SeqCst);
        Ok((
            content,
            request_id,
            model,
            usage,
            started.elapsed().as_millis() as u64,
        ))
    }
}

/// Remove benign default padding a model may add: keys that were not
/// intended, that map to an optional property in the normalized schema, and
/// whose value is either the schema's declared `default` or the natural zero
/// value of the property's type (`0`, `""`, `false`, `null`, `[]`). Optional
/// environment-dependent context fields (currently `cwd` on the shell server)
/// are also removed: the model cannot know the runtime allowed-directory
/// layout, so any value it pads there is context noise, not intent. Intended
/// keys are never touched, and everything is re-validated against the schema
/// by the caller before MCP execution.
pub fn strip_default_padding(
    model_args: Value,
    intended: &Value,
    normalized: &Normalized,
) -> Value {
    let Some(fields) = model_args.as_object() else {
        return model_args;
    };
    let schema_properties = normalized
        .schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let required: Vec<&str> = normalized
        .schema
        .get("required")
        .and_then(Value::as_array)
        .map(|req| req.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut fields = fields.clone();
    let keys: Vec<String> = fields.keys().cloned().collect();
    for key in keys {
        if intended.get(&key).is_some() {
            continue;
        }
        let Some(property) = schema_properties.get(&key) else {
            continue;
        };
        let value = fields.get(&key).expect("key present");
        let is_required = required.contains(&key.as_str());
        let equals_declared_default = property
            .get("default")
            .is_some_and(|default| value == default);
        let equals_zero_value = !is_required && matches_zero_value(value, property);
        let context_field = !is_required && key == "cwd";
        if equals_declared_default || equals_zero_value || context_field {
            fields.remove(&key);
        }
    }
    Value::Object(fields)
}

/// True when `value` equals the natural zero value of the property's
/// declared type in the normalized schema (`0` for numbers, `""` for
/// strings, `false` for booleans, `[]` for arrays, `null` otherwise).
fn matches_zero_value(value: &Value, property: &Value) -> bool {
    match property.get("type").and_then(Value::as_str) {
        Some("integer") | Some("number") => value.as_f64() == Some(0.0),
        Some("string") => value.as_str() == Some(""),
        Some("boolean") => value.as_bool() == Some(false),
        Some("array") => value.as_array().is_some_and(Vec::is_empty),
        _ => value.is_null(),
    }
}

/// Read a response body with a hard cap; errors when the bound is exceeded.
async fn read_bounded(response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    if let Some(len) = response.content_length()
        && len > limit as u64
    {
        return Err(format!("response body exceeds the {limit}-byte bound"));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("body read error: {e}"))?;
    if bytes.len() > limit {
        return Err(format!("response body exceeds the {limit}-byte bound"));
    }
    Ok(bytes.to_vec())
}

fn describe_bounded(value: &Value, cap: usize) -> String {
    let text = serde_json::to_string(value).unwrap_or_default();
    if text.chars().count() > cap {
        format!("{}…", text.chars().take(cap).collect::<String>())
    } else {
        text
    }
}

// ---------------------------------------------------------------------------
// Server spawning and discovery
// ---------------------------------------------------------------------------

/// Spawn one real server binary over stdio and drive the modern Discover
/// lifecycle. Returns the running client plus the discovered identity.
pub async fn spawn_server(args: &[&str]) -> Result<(Client, Discovered), String> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_modelcontextprotocol"));
    cmd.args(args);
    let client: Client = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(cmd).map_err(|e| format!("spawn failed: {e}"))?,
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .map_err(|e| format!("discover failed: {e}"))?;
    let info = client
        .peer_info()
        .ok_or_else(|| "discover returned no peer info".to_string())?;
    if info.protocol_version != ProtocolVersion::V_2026_07_28 {
        return Err(format!(
            "negotiated protocol {:?}, expected V_2026_07_28",
            info.protocol_version
        ));
    }
    let server_info = info
        .server_info
        .as_ref()
        .ok_or_else(|| "discover returned no server info".to_string())?;
    let discovered = Discovered {
        implementation_name: server_info.name.clone(),
        implementation_version: server_info.version.clone(),
        tools_capability: info.capabilities.tools.is_some(),
        resources_capability: info.capabilities.resources.is_some(),
        prompts_capability: info.capabilities.prompts.is_some(),
        instructions: info.instructions.clone().unwrap_or_default(),
    };
    Ok((client, discovered))
}

pub struct Discovered {
    pub implementation_name: String,
    pub implementation_version: String,
    pub tools_capability: bool,
    pub resources_capability: bool,
    pub prompts_capability: bool,
    pub instructions: String,
}

/// Validate discovery identity for one server and list its tools, prompts
/// and resources. Returns (client, tools, prompt names, resource URIs).
pub async fn discover_and_list(
    args: &[&str],
    expected_name: &str,
    expected_tools: bool,
    expected_prompts: bool,
    expected_resources: bool,
) -> Result<(Client, Vec<rmcp::model::Tool>, Vec<String>, Vec<String>), String> {
    let (client, discovered) = spawn_server(args).await?;
    if discovered.tools_capability != expected_tools {
        return Err(format!(
            "tools capability mismatch for {expected_name}: expected {expected_tools}"
        ));
    }
    if discovered.prompts_capability != expected_prompts {
        return Err(format!(
            "prompts capability mismatch for {expected_name}: expected {expected_prompts}"
        ));
    }
    if discovered.resources_capability != expected_resources {
        return Err(format!(
            "resources capability mismatch for {expected_name}: expected {expected_resources}"
        ));
    }
    if discovered.implementation_name != expected_name {
        return Err(format!(
            "server identity {:?} != expected {expected_name}",
            discovered.implementation_name
        ));
    }
    if discovered.implementation_version != env!("CARGO_PKG_VERSION") {
        return Err(format!(
            "server version {:?} != package version {}",
            discovered.implementation_version,
            env!("CARGO_PKG_VERSION")
        ));
    }
    if discovered.instructions.trim().is_empty() {
        return Err(format!(
            "{expected_name}: discover returned empty instructions"
        ));
    }

    let tools = client
        .list_all_tools()
        .await
        .map_err(|e| format!("tools/list failed: {e}"))?;
    let prompts = client
        .list_prompts(Default::default())
        .await
        .map_err(|e| format!("prompts/list failed: {e}"))?
        .prompts
        .into_iter()
        .map(|p| p.name.clone())
        .collect();
    let resources = client
        .list_resources(Default::default())
        .await
        .map_err(|e| format!("resources/list failed: {e}"))?
        .resources
        .into_iter()
        .map(|r| r.uri.clone())
        .collect();
    Ok((client, tools, prompts, resources))
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Filesystem fixture: a dedicated temp tree with deterministic content.
pub struct FsFixture {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
}

pub const A_TXT: &str = "alpha\nbeta\ngamma\n";

impl FsFixture {
    pub fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("mcp-e2e-fs-")
            .tempdir()
            .expect("create fs fixture");
        let root = dir.path().to_path_buf();
        std::fs::write(root.join("a.txt"), A_TXT).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/notes.md"), "# Title\n\nBody text.\n").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn lib() {}\n").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/skip.txt"), "skip\n").unwrap();
        std::fs::write(root.join("sub/d.rs"), "fn d() {}\n").unwrap();
        // Dedicated head-read fixture: some providers reproducibly corrupt the
        // compact "a.txt" + head:2 echo, so fs-002 reads this file instead.
        std::fs::write(root.join("sub/head.txt"), "first\nsecond\nthird\n").unwrap();
        let png = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
        )
        .unwrap();
        std::fs::write(root.join("pixel.png"), png).unwrap();
        std::fs::write(
            root.join("big.log"),
            (0..200).map(|i| format!("line {i}\n")).collect::<String>(),
        )
        .unwrap();
        std::fs::write(root.join("orig.txt"), "payload").unwrap();
        Self { _dir: dir, root }
    }
}

/// Memory fixture: a unique temp JSONL file.
pub struct MemFixture {
    _dir: tempfile::TempDir,
    pub file: PathBuf,
}

impl MemFixture {
    pub fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("mcp-e2e-mem-")
            .tempdir()
            .expect("create memory fixture");
        let file = dir.path().join("memory.jsonl");
        Self { _dir: dir, file }
    }
}

/// Shell fixture: isolated temp cwd with a `work` subdirectory.
pub struct ShellFixture {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
    pub work: PathBuf,
}

impl ShellFixture {
    pub fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("mcp-e2e-shell-")
            .tempdir()
            .expect("create shell fixture");
        let root = dir.path().to_path_buf();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        Self {
            _dir: dir,
            root,
            work,
        }
    }
}

/// Deterministic local fetch fixture (no public internet dependency).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchFixtureMode {
    /// /robots.txt returns 404 (allowed by convention).
    Plain,
    /// `User-agent: *\nAllow: /`
    AllowAll,
    /// `User-agent: *\nDisallow: /`
    DisallowAll,
}

/// The repeated sentence served at `/big`; 46 chars × 20 = 920 chars.
pub const BIG_PHRASE: &str = "The quick brown fox jumps over the lazy dog. ";

pub struct FetchFixture {
    pub base_url: String,
    _server: Arc<tiny_http::Server>,
}

fn fetch_response(
    status: u16,
    content_type: &str,
    body: String,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let bytes = body.into_bytes();
    let headers = vec![
        tiny_http::Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes())
            .expect("valid header"),
    ];
    tiny_http::Response::new(
        tiny_http::StatusCode(status),
        headers,
        std::io::Cursor::new(bytes.clone()),
        Some(bytes.len()),
        None,
    )
}

impl FetchFixture {
    pub fn start(mode: FetchFixtureMode) -> Self {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind fetch fixture"));
        let addr = server.server_addr().to_ip().expect("tcp listener");
        let robots = match mode {
            FetchFixtureMode::Plain => "MISSING".to_string(),
            FetchFixtureMode::AllowAll => "User-agent: *\nAllow: /\n".to_string(),
            FetchFixtureMode::DisallowAll => "User-agent: *\nDisallow: /\n".to_string(),
        };
        let server_thread = Arc::clone(&server);
        std::thread::spawn(move || {
            for request in server_thread.incoming_requests() {
                let url = request.url().to_string();
                let user_agent = request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("User-Agent"))
                    .map(|h| h.value.as_str().to_string())
                    .unwrap_or_default();
                let response = match url.as_str() {
                    "/robots.txt" if robots == "MISSING" => {
                        fetch_response(404, "text/plain", "not found".to_string())
                    }
                    "/robots.txt" => fetch_response(200, "text/plain", robots.clone()),
                    "/page" => fetch_response(
                        200,
                        "text/html; charset=utf-8",
                        "<!DOCTYPE html><html><head><title>Test Page</title></head>\
                         <body><article><h1>Hello World</h1><p>This is a test \
                         paragraph.</p></article></body></html>"
                            .to_string(),
                    ),
                    "/plain.txt" => fetch_response(
                        200,
                        "text/plain; charset=utf-8",
                        "plain text content\n".to_string(),
                    ),
                    "/big" => {
                        let body: String = BIG_PHRASE.repeat(20);
                        fetch_response(
                            200,
                            "text/html; charset=utf-8",
                            format!("<html><body><article>{body}</article></body></html>"),
                        )
                    }
                    "/echo-ua" => fetch_response(200, "text/plain; charset=utf-8", user_agent),
                    _ => fetch_response(404, "text/plain", "no such route".to_string()),
                };
                let _ = request.respond(response);
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            _server: server,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

/// Read the memory JSONL fixture from disk (persistence oracle).
pub fn memory_jsonl(file: &PathBuf) -> String {
    std::fs::read_to_string(file).unwrap_or_default()
}

/// Read a resource through MCP and return its text content.
pub async fn read_resource_text(client: &Client, uri: &str) -> Result<String, String> {
    let response = client
        .read_resource(ReadResourceRequestParams::new(uri.to_string()))
        .await
        .map_err(|e| format!("resources/read failed: {e}"))?;
    let text = response
        .contents
        .into_iter()
        .filter_map(|c| match c {
            rmcp::model::ResourceContents::TextResourceContents { text, .. } => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(text)
}

/// Retrieve the fetch prompt through MCP and return (first message text,
/// description).
pub async fn fetch_prompt_text(
    client: &Client,
    url: &str,
) -> Result<(String, Option<String>), String> {
    let result = client
        .get_prompt(
            GetPromptRequestParams::new("fetch").with_arguments(
                serde_json::json!({ "url": url })
                    .as_object()
                    .expect("object")
                    .clone(),
            ),
        )
        .await
        .map_err(|e| format!("prompts/get failed: {e}"))?;
    let text = result
        .messages
        .iter()
        .filter_map(|m| match &m.content {
            ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok((text, result.description.clone()))
}

// ---------------------------------------------------------------------------
// Tool executor
// ---------------------------------------------------------------------------

/// Executes tool calls against one live MCP client.
pub struct McpExecutor {
    pub client: Client,
}

impl McpExecutor {
    pub async fn call(&self, case: &Case, args: &Value) -> Result<CallToolResult, String> {
        self.client
            .call_tool(
                CallToolRequestParams::new(case.tool.to_string())
                    .with_arguments(args.as_object().cloned().unwrap_or_default()),
            )
            .await
            .map_err(|e| format!("MCP call failed: {e}"))
    }
}

/// MCP result → tool-message content, capped to keep token cost bounded.
pub fn mcp_result_to_text(result: &CallToolResult, cap: usize) -> String {
    let mut text = String::new();
    for block in &result.content {
        match block {
            ContentBlock::Text(t) => text.push_str(&t.text),
            ContentBlock::Image(i) => {
                text.push_str(&format!(
                    "[image {} ({} decoded bytes)]",
                    i.mime_type,
                    i.data.len() / 4 * 3
                ));
            }
            ContentBlock::Audio(a) => {
                text.push_str(&format!(
                    "[audio {} ({} decoded bytes)]",
                    a.mime_type,
                    a.data.len() / 4 * 3
                ));
            }
            ContentBlock::Resource(r) => match &r.resource {
                rmcp::model::ResourceContents::TextResourceContents {
                    uri, text: inner, ..
                } => {
                    text.push_str(&format!("[resource {uri}]: {inner}"));
                }
                rmcp::model::ResourceContents::BlobResourceContents { uri, blob, .. } => {
                    text.push_str(&format!(
                        "[resource {uri}: {} blob bytes]",
                        blob.len() / 4 * 3
                    ));
                }
                _ => {}
            },
            other => text.push_str(&format!("[{other:?}]")),
        }
        text.push('\n');
    }
    if let Some(structured) = &result.structured_content {
        text.push_str(&serde_json::to_string(structured).unwrap_or_default());
    }
    if text.chars().count() > cap {
        text = text.chars().take(cap).collect();
    }
    text
}

// ---------------------------------------------------------------------------
// Case execution driver
// ---------------------------------------------------------------------------

/// Execute one case against a live client: OpenRouter forced roundtrip plus
/// the case oracle. Records a metric; returns Ok(()) or a sanitized failure
/// description.
pub async fn run_case(
    client: &OpenRouterClient,
    case: &Case,
    ctx: &CaseCtx,
    tools: &[rmcp::model::Tool],
    metrics: Arc<Metrics>,
    executor: &McpExecutor,
    fetch_base: Option<&str>,
) -> Result<(), String> {
    let Some(runtime_tool) = tools.iter().find(|t| t.name == case.tool) else {
        return Err(format!("runtime tool {:?} not found", case.tool));
    };
    let normalized = normalize_tool_schema(&runtime_tool.schema_as_json_value());
    for diagnostic in &normalized.diagnostics {
        metrics.diagnostic(format!("{}: {}", case.tool, diagnostic));
    }
    let function = Value::Object(Map::from_iter([
        ("type".to_string(), Value::String("function".to_string())),
        (
            "function".to_string(),
            Value::Object(Map::from_iter([
                ("name".to_string(), Value::String(case.tool.to_string())),
                (
                    "description".to_string(),
                    Value::String(
                        runtime_tool
                            .description
                            .clone()
                            .unwrap_or_default()
                            .into_owned(),
                    ),
                ),
                ("parameters".to_string(), normalized.schema.clone()),
            ])),
        ),
    ]));

    let intended = resolve_case_args(case, ctx, fetch_base);
    let started = Instant::now();
    let mut roundtrip = client
        .forced_roundtrip(case, &function, &normalized, &intended, executor)
        .await;
    // Model-generation flakes are retried a bounded number of times: the
    // model is non-deterministic even at temperature 0 and occasionally
    // drops or rewrites an argument, emits truncated (unparseable JSON)
    // arguments, or consumes its token budget on hidden generation without
    // emitting the forced tool call or a visible final answer. The argument
    // guard runs on every attempt and blocks before any MCP execution, so a
    // retry can never execute unvalidated arguments. Oracle, transport and
    // HTTP failures are never retried.
    const MAX_GENERATION_RETRIES: u32 = 2;
    let mut generation_retries: u32 = 0;
    while generation_retries < MAX_GENERATION_RETRIES
        && matches!(&roundtrip, Err(e) if is_generation_flake(e))
    {
        generation_retries += 1;
        let note: String = roundtrip
            .as_ref()
            .err()
            .map(|e| e.chars().take(200).collect())
            .unwrap_or_default();
        metrics.diagnostic(format!(
            "case {} ({}): model generation flake, retrying \
             ({generation_retries}/{MAX_GENERATION_RETRIES}): {note}",
            case.id, case.tool
        ));
        metrics.counters.echo_retries.fetch_add(1, Ordering::SeqCst);
        roundtrip = client
            .forced_roundtrip(case, &function, &normalized, &intended, executor)
            .await;
    }
    let elapsed_ms = started.elapsed().as_millis() as u64;

    let mut metric = super::report::CaseMetric {
        case_id: case.id,
        server: super::cases::server_name(case.server),
        tool: case.tool,
        status: "ok",
        elapsed_ms,
        mcp_ok: roundtrip
            .as_ref()
            .map(|r| r.mcp_result.is_error != Some(true))
            .unwrap_or(false),
        roundtrip: roundtrip.is_ok(),
        note: case.note.to_string(),
        ..Default::default()
    };

    let result = match roundtrip {
        Ok(roundtrip) => {
            metrics.counters.tool_calls.fetch_add(1, Ordering::SeqCst);
            metrics
                .counters
                .full_roundtrips
                .fetch_add(1, Ordering::SeqCst);
            metric.request_ids = roundtrip.request_ids;
            metric.actual_models = roundtrip.actual_models;
            metric.usage = roundtrip.usage;
            metric.retries = roundtrip.retries;
            // Independent programmatic oracle over the MCP result.
            match oracle_check(&case.oracle, &roundtrip.mcp_result, ctx) {
                Ok(()) => Ok(()),
                Err(e) => {
                    metric.status = "error";
                    Err(format!(
                        "case {} ({}): oracle failed: {e}",
                        case.id, case.tool
                    ))
                }
            }
        }
        Err(e) => {
            metric.status = if e.contains("deviation") {
                "deviation"
            } else {
                "error"
            };
            Err(format!("case {} ({}): {e}", case.id, case.tool))
        }
    };
    metrics.record(metric);
    result
}

/// Whether an error from `forced_roundtrip` is a retryable model-generation
/// flake rather than a real contract, oracle, or transport failure.
fn is_generation_flake(error: &str) -> bool {
    // The model dropped, rewrote, or padded an argument (the argument guard
    // re-runs on every attempt and still blocks before MCP execution).
    error.starts_with("deviation:")
        // The model emitted truncated arguments that are not valid JSON.
        || error.contains("tool arguments are not valid JSON")
        // The model exhausted its token budget without emitting the forced
        // tool call (finish_reason "length").
        || (error.contains("no tool_calls in assistant message")
            && error.contains("finish_reason")
            && error.contains("length"))
        // The final answer was cut off by the token ceiling, producing an
        // empty visible response.
        || error.contains("final assistant response length 0 outside")
}

pub fn oracle_check(oracle: &Oracle, result: &CallToolResult, ctx: &CaseCtx) -> Result<(), String> {
    let text = super::cases::result_text(result);
    let err = |message: String| -> Result<(), String> {
        Err(format!(
            "{message} (is_error={:?}, text: {})",
            result.is_error,
            describe_text(&text, 300)
        ))
    };
    match oracle {
        Oracle::Ok => {
            if result.is_error == Some(true) {
                err("expected success, got a tool error".to_string())
            } else {
                Ok(())
            }
        }
        Oracle::ErrTextContains(needle) => {
            if result.is_error != Some(true) {
                err(format!("expected a tool error containing {needle:?}"))
            } else if text.contains(needle) {
                Ok(())
            } else {
                err(format!("expected error text containing {needle:?}"))
            }
        }
        Oracle::TextContains(needles) => {
            for needle in *needles {
                if !text.contains(needle) {
                    return err(format!("expected text containing {needle:?}"));
                }
            }
            Ok(())
        }
        Oracle::TextNotContains(forbidden) => {
            for needle in *forbidden {
                if text.contains(needle) {
                    return err(format!("expected text NOT containing {needle:?}"));
                }
            }
            Ok(())
        }
        Oracle::TextEquals(expected) => {
            if text == *expected {
                Ok(())
            } else {
                err(format!("expected text {:?}", describe_text(expected, 200)))
            }
        }
        Oracle::Custom(check) => check(result, ctx),
    }
}

fn describe_text(text: &str, cap: usize) -> String {
    if text.chars().count() > cap {
        format!("{}…", text.chars().take(cap).collect::<String>())
    } else {
        text.to_string()
    }
}

/// Substitute runtime-dependent placeholders in case args:
/// `{base}` (fetch fixture URL) and `{bin}` / `{helper}` (executable paths).
pub fn resolve_case_args(case: &Case, ctx: &CaseCtx, fetch_base: Option<&str>) -> Value {
    let mut args = case.args.clone();
    if let Some(base) = fetch_base
        && let Some(rendered) = render_strings(&args, "{base}", base)
    {
        args = rendered;
    }
    if let Some(rendered) = render_strings(&args, "{bin}", ctx.bin) {
        args = rendered;
    }
    if let Some(rendered) = render_strings(&args, "{helper}", &ctx.helper) {
        args = rendered;
    }
    args
}

fn render_strings(value: &Value, needle: &str, replacement: &str) -> Option<Value> {
    match value {
        Value::String(s) if s.contains(needle) => {
            Some(Value::String(s.replace(needle, replacement)))
        }
        Value::Array(items) => {
            let rendered: Vec<Value> = items
                .iter()
                .map(|item| {
                    render_strings(item, needle, replacement).unwrap_or_else(|| item.clone())
                })
                .collect();
            if rendered.iter().zip(items.iter()).any(|(r, i)| r != i) {
                Some(Value::Array(rendered))
            } else {
                None
            }
        }
        Value::Object(map) => {
            let mut out = map.clone();
            let mut changed = false;
            for (key, item) in map {
                if let Some(rendered) = render_strings(item, needle, replacement) {
                    out.insert(key.clone(), rendered);
                    changed = true;
                }
            }
            if changed {
                Some(Value::Object(out))
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalized(properties: Value, required: Value) -> Normalized {
        Normalized {
            schema: json!({
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false,
            }),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn strips_zero_value_padding_for_optional_numeric_fields() {
        // Filesystem read_text_file: optional head/tail the model pads with 0.
        let norm = normalized(
            json!({
                "path": { "type": "string" },
                "head": { "type": "integer", "minimum": 0 },
                "tail": { "type": "integer", "minimum": 0 }
            }),
            json!(["path"]),
        );
        // Model padded head:0 alongside intended tail:2.
        assert_eq!(
            strip_default_padding(
                json!({"head": 0, "path": "a.txt", "tail": 2}),
                &json!({"path": "a.txt", "tail": 2}),
                &norm,
            ),
            json!({"path": "a.txt", "tail": 2}),
        );
        // Model padded both head:0 and tail:0 alongside intended path only.
        assert_eq!(
            strip_default_padding(
                json!({"head": 0, "path": "a.txt", "tail": 0}),
                &json!({"path": "a.txt"}),
                &norm,
            ),
            json!({"path": "a.txt"}),
        );
    }

    #[test]
    fn strips_empty_and_environment_context_cwd_padding() {
        // Shell execute_command: optional cwd; model pads "" or a real path.
        let norm = normalized(
            json!({
                "program": { "type": "string" },
                "args": { "type": "array", "items": { "type": "string" } },
                "cwd": { "type": "string" }
            }),
            json!(["program"]),
        );
        assert_eq!(
            strip_default_padding(
                json!({"cwd": "", "program": "/bin/true"}),
                &json!({"program": "/bin/true"}),
                &norm,
            ),
            json!({"program": "/bin/true"}),
        );
        assert_eq!(
            strip_default_padding(
                json!({"cwd": "/some/absolute/path", "program": "/bin/true", "args": ["--version"]}),
                &json!({"program": "/bin/true", "args": ["--version"]}),
                &norm,
            ),
            json!({"program": "/bin/true", "args": ["--version"]}),
        );
    }

    #[test]
    fn never_strips_required_or_intended_keys() {
        let norm = normalized(
            json!({
                "path": { "type": "string" },
                "head": { "type": "integer" }
            }),
            json!(["path"]),
        );
        // Required key with zero value is never stripped.
        assert_eq!(
            strip_default_padding(
                json!({"path": "", "head": 0}),
                &json!({"path": "", "head": 0}),
                &norm,
            ),
            json!({"path": "", "head": 0}),
        );
        // An intended head:0 is preserved even though it is a zero value.
        assert_eq!(
            strip_default_padding(
                json!({"path": "a.txt", "head": 0}),
                &json!({"path": "a.txt", "head": 0}),
                &norm,
            ),
            json!({"path": "a.txt", "head": 0}),
        );
    }

    #[test]
    fn strips_schema_declared_defaults_and_nonzero_is_kept() {
        let norm = normalized(
            json!({
                "path": { "type": "string" },
                "limit": { "type": "integer", "default": 100 }
            }),
            json!(["path"]),
        );
        assert_eq!(
            strip_default_padding(
                json!({"path": "a.txt", "limit": 100}),
                &json!({"path": "a.txt"}),
                &norm,
            ),
            json!({"path": "a.txt"}),
        );
        // A non-default, non-zero padding value is a real deviation.
        assert_eq!(
            strip_default_padding(
                json!({"path": "a.txt", "limit": 7}),
                &json!({"path": "a.txt"}),
                &norm,
            ),
            json!({"path": "a.txt", "limit": 7}),
        );
    }
}
