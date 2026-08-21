use std::{fmt, path::Path};

use reqwest::{Client, StatusCode, redirect::Policy};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{
    activity::{ActivityPhase, ActivityReporter, AgentActivityEvent, bound},
    child_mcp::{ChildCallError, ChildMcpManager, ChildTool},
    definition::{AgentDefinition, WireApi},
    timeouts::{PROVIDER_CONNECT_TIMEOUT, PROVIDER_REQUEST_TIMEOUT},
};

const MAX_TOKENS: u32 = 8192;
const ERROR_MESSAGE_LIMIT: usize = 256;
const MAX_PROVIDER_BODY_BYTES: usize = 8 * 1024 * 1024;

/// An API credential deliberately does not implement `Display` and only emits a
/// redacted representation when logged.
pub(crate) struct ProviderCredential(String);

impl fmt::Debug for ProviderCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderCredential([REDACTED])")
    }
}

impl ProviderCredential {
    pub(crate) fn resolve(definition: &AgentDefinition) -> Result<Self, ProviderError> {
        Self::resolve_with(&definition.env_key, |key| std::env::var(key).ok())
    }

    fn resolve_with<F>(env_key: &str, resolve: F) -> Result<Self, ProviderError>
    where
        F: FnOnce(&str) -> Option<String>,
    {
        match resolve(env_key) {
            Some(value) if !value.is_empty() => Ok(Self(value)),
            _ => Err(ProviderError::missing_environment_variable(env_key)),
        }
    }
}

#[derive(Clone)]
pub(crate) enum ConversationState {
    Responses(Vec<Value>),
    AnthropicMessages(Vec<Value>),
}

impl ConversationState {
    pub(crate) fn new(wire_api: &WireApi) -> Self {
        match wire_api {
            WireApi::Responses => Self::Responses(Vec::new()),
            WireApi::AnthropicMessages => Self::AnthropicMessages(Vec::new()),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ProviderError {
    pub kind: &'static str,
    pub message: String,
    pub resumable: bool,
}

impl ProviderError {
    fn missing_environment_variable(env_key: &str) -> Self {
        Self {
            kind: "missing_environment_variable",
            message: format!("Required environment variable {env_key} is not available."),
            resumable: false,
        }
    }

    fn provider(message: impl Into<String>) -> Self {
        Self {
            kind: "provider_error",
            message: bounded_message(&message.into()),
            resumable: true,
        }
    }

    fn interrupted() -> Self {
        Self {
            kind: "run_interrupted",
            message: "agent run was interrupted".into(),
            resumable: true,
        }
    }

    fn context_limit() -> Self {
        Self {
            kind: "context_limit",
            message: "The retained agent conversation no longer fits the selected model context."
                .into(),
            resumable: true,
        }
    }

    fn child_error() -> Self {
        Self {
            kind: "child_mcp_tool_error",
            message: "child MCP tool call failed".into(),
            resumable: true,
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProviderError {}

pub(crate) struct ProviderClient {
    client: Client,
}

impl ProviderClient {
    pub(crate) fn new() -> Result<Self, ProviderError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = Client::builder()
            .connect_timeout(PROVIDER_CONNECT_TIMEOUT)
            .timeout(PROVIDER_REQUEST_TIMEOUT)
            .redirect(Policy::none())
            .build()
            .map_err(|_| ProviderError::provider("unable to create provider HTTP client"))?;
        Ok(Self { client })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run(
        &self,
        definition: &AgentDefinition,
        credential: &ProviderCredential,
        system_context: &str,
        user_message: &str,
        state: &mut ConversationState,
        child: &ChildMcpManager,
        cancel: &CancellationToken,
        reporter: &ActivityReporter,
        workspace: &Path,
    ) -> Result<String, ProviderError> {
        if user_message.trim().is_empty() {
            return Err(ProviderError::provider("user message must not be empty"));
        }
        match (&definition.wire_api, state) {
            (WireApi::Responses, ConversationState::Responses(history)) => {
                history.push(responses_user_message(user_message));
                self.run_responses(
                    definition,
                    credential,
                    system_context,
                    history,
                    child,
                    cancel,
                    reporter,
                    workspace,
                )
                .await
            }
            (WireApi::AnthropicMessages, ConversationState::AnthropicMessages(history)) => {
                history.push(anthropic_user_message(user_message));
                self.run_anthropic(
                    definition,
                    credential,
                    system_context,
                    history,
                    child,
                    cancel,
                    reporter,
                    workspace,
                )
                .await
            }
            _ => Err(ProviderError::provider(
                "conversation state does not match provider wire API",
            )),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_responses(
        &self,
        definition: &AgentDefinition,
        credential: &ProviderCredential,
        system_context: &str,
        history: &mut Vec<Value>,
        child: &ChildMcpManager,
        cancel: &CancellationToken,
        reporter: &ActivityReporter,
        workspace: &Path,
    ) -> Result<String, ProviderError> {
        let endpoint = endpoint(&definition.base_url, "responses")?;
        for _ in 0..definition.max_turns {
            reporter
                .report(AgentActivityEvent {
                    phase: ActivityPhase::Model,
                    summary: "Waiting for model response".into(),
                    target: None,
                    tool: None,
                    deadline: Some(std::time::Instant::now() + PROVIDER_REQUEST_TIMEOUT),
                    kind: "model_started",
                })
                .await;
            let request = responses_request(definition, system_context, history, child.tools());
            let response = self
                .post_json(
                    endpoint.clone(),
                    request,
                    credential,
                    WireApi::Responses,
                    cancel,
                )
                .await?;
            let parsed = parse_responses_response(response)?;
            if parsed.calls.is_empty() {
                commit_responses(history, parsed.items, Vec::new());
                return parsed
                    .text
                    .filter(|text| !text.trim().is_empty())
                    .ok_or_else(|| ProviderError::provider("provider returned an empty response"));
            }
            let outputs = execute_calls(child, parsed.calls, cancel, reporter, workspace).await?;
            commit_responses(history, parsed.items, responses_tool_outputs(outputs));
        }
        Err(ProviderError::provider(
            "provider exceeded the configured turn limit",
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_anthropic(
        &self,
        definition: &AgentDefinition,
        credential: &ProviderCredential,
        system_context: &str,
        history: &mut Vec<Value>,
        child: &ChildMcpManager,
        cancel: &CancellationToken,
        reporter: &ActivityReporter,
        workspace: &Path,
    ) -> Result<String, ProviderError> {
        let endpoint = endpoint(&definition.base_url, "v1/messages")?;
        for _ in 0..definition.max_turns {
            reporter
                .report(AgentActivityEvent {
                    phase: ActivityPhase::Model,
                    summary: "Waiting for model response".into(),
                    target: None,
                    tool: None,
                    deadline: Some(std::time::Instant::now() + PROVIDER_REQUEST_TIMEOUT),
                    kind: "model_started",
                })
                .await;
            let request = anthropic_request(definition, system_context, history, child.tools());
            let response = self
                .post_json(
                    endpoint.clone(),
                    request,
                    credential,
                    WireApi::AnthropicMessages,
                    cancel,
                )
                .await?;
            let parsed = parse_anthropic_response(response)?;
            if parsed.calls.is_empty() {
                commit_anthropic(history, parsed.content, Vec::new());
                if matches!(
                    parsed.stop_reason.as_deref(),
                    Some("max_tokens" | "refusal" | "error")
                ) {
                    return Err(ProviderError::provider(
                        "provider stopped without a usable response",
                    ));
                }
                if parsed.stop_reason.as_deref() == Some("pause_turn") {
                    continue;
                }
                return parsed
                    .text
                    .filter(|text| !text.trim().is_empty())
                    .ok_or_else(|| ProviderError::provider("provider returned an empty response"));
            }
            let outputs = execute_calls(child, parsed.calls, cancel, reporter, workspace).await?;
            commit_anthropic(history, parsed.content, anthropic_tool_results(outputs));
        }
        Err(ProviderError::provider(
            "provider exceeded the configured turn limit",
        ))
    }

    async fn post_json(
        &self,
        endpoint: Url,
        request: Value,
        credential: &ProviderCredential,
        wire_api: WireApi,
        cancel: &CancellationToken,
    ) -> Result<Value, ProviderError> {
        let body = serialize_provider_body(&request)?;
        let request = match wire_api {
            WireApi::Responses => self.client.post(endpoint).bearer_auth(&credential.0),
            WireApi::AnthropicMessages => self
                .client
                .post(endpoint)
                .header("x-api-key", &credential.0)
                .header("anthropic-version", "2023-06-01"),
        }
        .header("content-type", "application/json")
        .body(body);
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err(ProviderError::interrupted()),
            result = request.send() => result.map_err(request_error)?,
        };
        let status = response.status();
        if status != StatusCode::PAYLOAD_TOO_LARGE
            && response
                .content_length()
                .is_some_and(|length| length > MAX_PROVIDER_BODY_BYTES as u64)
        {
            return Err(ProviderError::provider(
                "provider response body exceeds the size limit",
            ));
        }
        let mut response = response;
        let mut body = Vec::new();
        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => return Err(ProviderError::interrupted()),
                result = response.chunk() => result.map_err(request_error)?,
            };
            let Some(chunk) = chunk else {
                break;
            };
            append_provider_bytes(&mut body, &chunk)?;
        }
        if !status.is_success() {
            return Err(status_error(status, &body));
        }
        serde_json::from_slice(&body)
            .map_err(|_| ProviderError::provider("provider returned malformed JSON"))
    }
}

struct ToolCall {
    id: String,
    name: String,
    arguments: Value,
}

struct ToolResult {
    id: String,
    output: String,
}

struct ResponsesOutput {
    items: Vec<Value>,
    calls: Vec<ToolCall>,
    text: Option<String>,
}

struct AnthropicOutput {
    content: Vec<Value>,
    calls: Vec<ToolCall>,
    text: Option<String>,
    stop_reason: Option<String>,
}

async fn execute_calls(
    child: &ChildMcpManager,
    calls: Vec<ToolCall>,
    cancel: &CancellationToken,
    reporter: &ActivityReporter,
    workspace: &Path,
) -> Result<Vec<ToolResult>, ProviderError> {
    let mut outputs = Vec::with_capacity(calls.len());
    for call in calls {
        if cancel.is_cancelled() {
            return Err(ProviderError::interrupted());
        }
        let (summary, tool, target) = safe_tool_activity(&call.name, &call.arguments, workspace);
        reporter
            .report(AgentActivityEvent::tool(summary, tool, target))
            .await;
        let result = child.call(&call.name, call.arguments, cancel).await;
        match result.as_ref().err() {
            Some(ChildCallError::TimedOut) => {
                reporter.report(AgentActivityEvent::tool_timed_out()).await
            }
            Some(ChildCallError::Failed) => {
                reporter.report(AgentActivityEvent::tool_failed()).await
            }
            _ => reporter.report(AgentActivityEvent::tool_completed()).await,
        }
        let output = match result {
            Ok(output) => output,
            Err(ChildCallError::Interrupted) => return Err(ProviderError::interrupted()),
            Err(_) => return Err(ProviderError::child_error()),
        };
        outputs.push(ToolResult {
            id: call.id,
            output,
        });
    }
    Ok(outputs)
}

fn safe_tool_activity(
    name: &str,
    arguments: &Value,
    workspace: &Path,
) -> (String, String, Option<String>) {
    let name = safe_name(name);
    let object = arguments.as_object();
    if let Some(command) = object.and_then(|value| value.get("command")) {
        if let Some(command) = safe_command(command) {
            return (format!("Running {command}"), name, None);
        }
        return ("Running shell command".into(), name, None);
    }
    let path = object
        .and_then(|value| value.get("path").or_else(|| value.get("file_path")))
        .and_then(Value::as_str)
        .and_then(|value| safe_workspace_path(value, workspace));
    let lower = name.to_ascii_lowercase();
    let operation = lower
        .rsplit_once("__")
        .map_or(lower.as_str(), |(_, operation)| operation);
    let operation = operation
        .rsplit_once('.')
        .map_or(operation, |(_, operation)| operation);
    let operation = operation.rsplit('/').next().unwrap_or(operation);
    let action = if operation == "read"
        || operation.starts_with("read_")
        || operation.starts_with("get_file")
    {
        Some("Reading")
    } else if operation == "write"
        || operation.starts_with("write_")
        || operation.starts_with("edit_")
        || operation.starts_with("create_file")
        || operation.starts_with("move_file")
    {
        Some("Writing")
    } else if operation == "search"
        || operation.starts_with("search_")
        || operation.starts_with("grep")
        || operation.starts_with("find")
        || operation.starts_with("list_")
        || operation.starts_with("directory_tree")
    {
        Some("Searching")
    } else {
        None
    };
    match (action, path) {
        (Some(action), Some(path)) => (format!("{action} {path}"), name, Some(path)),
        _ => (format!("Calling {name}"), name, None),
    }
}
fn safe_name(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
        .collect();
    bound(
        if cleaned.is_empty() {
            "tool".into()
        } else {
            cleaned
        },
        80,
    )
}
fn safe_workspace_path(value: &str, workspace: &Path) -> Option<String> {
    let path = Path::new(value);
    let relative = if path.is_absolute() {
        path.strip_prefix(workspace).ok()?
    } else {
        path
    };
    (!relative.as_os_str().is_empty()
        && !relative
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir)))
    .then(|| bound(relative.to_string_lossy().replace('\\', "/"), 120))
}
fn safe_command(value: &Value) -> Option<String> {
    let command = match value {
        Value::String(value) => value.trim().to_owned(),
        Value::Array(parts) => parts
            .iter()
            .take(2)
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()?
            .join(" "),
        _ => return None,
    };
    let allowed = [
        "cargo test",
        "cargo clippy",
        "cargo check",
        "cargo build",
        "cargo fmt",
        "npm test",
        "pnpm test",
        "yarn test",
        "pytest",
        "go test",
    ];
    allowed
        .iter()
        .find(|prefix| {
            command == **prefix
                || command
                    .strip_prefix(**prefix)
                    .is_some_and(|rest| rest.starts_with(char::is_whitespace))
        })
        .map(|prefix| (*prefix).to_owned())
}

fn responses_tool_outputs(results: Vec<ToolResult>) -> Vec<Value> {
    results
        .into_iter()
        .map(|result| {
            json!({
                "type": "function_call_output",
                "call_id": result.id,
                "output": result.output,
            })
        })
        .collect()
}

fn anthropic_tool_results(results: Vec<ToolResult>) -> Vec<Value> {
    results
        .into_iter()
        .map(|result| {
            json!({
                "type": "tool_result",
                "tool_use_id": result.id,
                "content": result.output,
            })
        })
        .collect()
}

/// Commits a Responses turn only after every function call has a matching
/// output, keeping replay history valid if a child call fails or is cancelled.
fn commit_responses(history: &mut Vec<Value>, items: Vec<Value>, outputs: Vec<Value>) {
    history.extend(items);
    history.extend(outputs);
}

/// Commits the assistant turn and, when applicable, its complete batch of
/// tool results as the single user turn required by the Messages wire format.
fn commit_anthropic(history: &mut Vec<Value>, content: Vec<Value>, results: Vec<Value>) {
    history.push(json!({ "role": "assistant", "content": content }));
    if !results.is_empty() {
        history.push(json!({ "role": "user", "content": results }));
    }
}

fn responses_request(
    definition: &AgentDefinition,
    instructions: &str,
    input: &[Value],
    tools: &[ChildTool],
) -> Value {
    let mut request = json!({
        "model": definition.model,
        "instructions": instructions,
        "input": input,
        "tools": responses_tools(tools),
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "store": false,
    });
    if let Some(effort) = &definition.reasoning_effort {
        request["reasoning"] = json!({ "effort": effort });
    }
    if let Some(temperature) = definition.temperature {
        request["temperature"] = json!(temperature);
    }
    request
}

fn anthropic_request(
    definition: &AgentDefinition,
    system: &str,
    messages: &[Value],
    tools: &[ChildTool],
) -> Value {
    let mut request = json!({
        "model": definition.model,
        "max_tokens": MAX_TOKENS,
        "system": system,
        "messages": messages,
        "tools": anthropic_tools(tools),
    });
    if let Some(temperature) = definition.temperature {
        request["temperature"] = json!(temperature);
    }
    if let Some(effort) = &definition.reasoning_effort {
        request["output_config"] = json!({ "effort": effort });
    }
    request
}

fn responses_tools(tools: &[ChildTool]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({ "type": "function", "name": tool.provider_name, "description": tool.description, "parameters": (*tool.input_schema).clone() })
        })
        .collect()
}

fn anthropic_tools(tools: &[ChildTool]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({ "name": tool.provider_name, "description": tool.description, "input_schema": (*tool.input_schema).clone() })
        })
        .collect()
}

fn responses_user_message(message: &str) -> Value {
    json!({ "role": "user", "content": [{ "type": "input_text", "text": message }] })
}

fn anthropic_user_message(message: &str) -> Value {
    json!({ "role": "user", "content": message })
}

fn parse_responses_response(value: Value) -> Result<ResponsesOutput, ProviderError> {
    let object = value
        .as_object()
        .ok_or_else(|| ProviderError::provider("provider returned an invalid response"))?;
    let items = object
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| ProviderError::provider("provider response omitted output"))?;
    let mut calls = Vec::new();
    let mut block_text = String::new();
    for item in &items {
        if item.get("type").and_then(Value::as_str) == Some("function_call") {
            let id = required_string(item, "call_id")?;
            let name = required_string(item, "name")?;
            let raw_arguments = required_string(item, "arguments")?;
            let arguments: Value = serde_json::from_str(&raw_arguments)
                .map_err(|_| ProviderError::provider("provider returned invalid tool arguments"))?;
            calls.push(ToolCall {
                id,
                name,
                arguments,
            });
        }
        collect_response_text(item, &mut block_text);
    }
    let text = object
        .get("output_text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| (!block_text.is_empty()).then_some(block_text));
    Ok(ResponsesOutput { items, calls, text })
}

fn parse_anthropic_response(value: Value) -> Result<AnthropicOutput, ProviderError> {
    let object = value
        .as_object()
        .ok_or_else(|| ProviderError::provider("provider returned an invalid response"))?;
    let content = object
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| ProviderError::provider("provider response omitted content"))?;
    let mut calls = Vec::new();
    let mut text = String::new();
    for block in &content {
        match block.get("type").and_then(Value::as_str) {
            Some("tool_use") => calls.push(ToolCall {
                id: required_string(block, "id")?,
                name: required_string(block, "name")?,
                arguments: block
                    .get("input")
                    .cloned()
                    .ok_or_else(|| ProviderError::provider("provider tool call omitted input"))?,
            }),
            Some("text") => {
                if let Some(value) = block.get("text").and_then(Value::as_str) {
                    text.push_str(value);
                }
            }
            _ => {}
        }
    }
    Ok(AnthropicOutput {
        content,
        calls,
        text: (!text.is_empty()).then_some(text),
        stop_reason: object
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn collect_response_text(item: &Value, output: &mut String) {
    if item.get("type").and_then(Value::as_str) == Some("message")
        && let Some(content) = item.get("content").and_then(Value::as_array)
    {
        for block in content {
            if block.get("type").and_then(Value::as_str) == Some("output_text")
                && let Some(text) = block.get("text").and_then(Value::as_str)
            {
                output.push_str(text);
            }
        }
    }
}

fn required_string(value: &Value, key: &str) -> Result<String, ProviderError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ProviderError::provider("provider returned a malformed tool call"))
}

fn endpoint(base_url: &Url, suffix: &str) -> Result<Url, ProviderError> {
    let mut base = base_url.clone();
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    base.join(suffix)
        .map_err(|_| ProviderError::provider("invalid provider endpoint"))
}

fn request_error(error: reqwest::Error) -> ProviderError {
    if error.is_timeout() {
        ProviderError {
            kind: "provider_timeout",
            message: "provider request timed out".into(),
            resumable: true,
        }
    } else {
        ProviderError::provider("provider request failed")
    }
}

fn status_error(status: StatusCode, body: &[u8]) -> ProviderError {
    if status == StatusCode::PAYLOAD_TOO_LARGE || provider_reports_context_limit(body) {
        return ProviderError::context_limit();
    }
    ProviderError::provider(format!("provider returned HTTP status {}", status.as_u16()))
}

fn provider_reports_context_limit(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let Some(error) = value.get("error") else {
        return false;
    };
    let code = error
        .get("code")
        .or_else(|| error.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    code.contains("context_length")
        || code.contains("context_window")
        || message.contains("maximum context length")
        || message.contains("context window")
        || message.contains("too many tokens")
}

fn bounded_message(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    if cleaned.len() <= ERROR_MESSAGE_LIMIT {
        return cleaned;
    }
    let mut end = ERROR_MESSAGE_LIMIT - '…'.len_utf8();
    while !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &cleaned[..end])
}

fn serialize_provider_body(value: &Value) -> Result<String, ProviderError> {
    let body = serde_json::to_string(value)
        .map_err(|_| ProviderError::provider("unable to serialize provider request"))?;
    if body.len() > MAX_PROVIDER_BODY_BYTES {
        return Err(ProviderError::context_limit());
    }
    Ok(body)
}

fn append_provider_bytes(body: &mut Vec<u8>, chunk: &[u8]) -> Result<(), ProviderError> {
    let remaining = MAX_PROVIDER_BODY_BYTES.saturating_sub(body.len());
    if chunk.len() > remaining {
        return Err(ProviderError::provider(
            "provider response body exceeds the size limit",
        ));
    }
    body.extend_from_slice(chunk);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, path::PathBuf};

    use super::super::definition::{
        IsolationMode, ModelProviderKind, PermissionPolicy, SandboxMode,
    };

    fn definition(wire_api: WireApi) -> AgentDefinition {
        AgentDefinition {
            name: "test".into(),
            description: String::new(),
            instructions: String::new(),
            model: "model".into(),
            provider: ModelProviderKind::Custom,
            base_url: Url::parse("https://example.test/v1").unwrap(),
            env_key: "TEST_KEY".into(),
            wire_api,
            reasoning_effort: Some("high".into()),
            temperature: Some(0.5),
            max_turns: 2,
            permission: PermissionPolicy::default(),
            sandbox: SandboxMode::Default,
            isolation: IsolationMode::None,
            skills: Vec::new(),
            mcp_servers: BTreeMap::new(),
            source_path: PathBuf::new(),
        }
    }

    #[test]
    fn credentials_are_redacted_and_resolved_without_environment_mutation() {
        let credential =
            ProviderCredential::resolve_with("TEST_KEY", |_| Some("secret-value".into())).unwrap();
        assert!(!format!("{credential:?}").contains("secret-value"));
        let error =
            ProviderCredential::resolve_with("TEST_KEY", |_| Some(String::new())).unwrap_err();
        assert_eq!(error.kind, "missing_environment_variable");
        assert_eq!(
            error.message,
            "Required environment variable TEST_KEY is not available."
        );
    }

    #[test]
    fn parses_responses_items_and_preserves_reasoning() {
        let parsed = parse_responses_response(json!({"output":[
            {"type":"reasoning","summary":[],"encrypted_content":"opaque"},
            {"type":"function_call","call_id":"call_1","name":"tool","arguments":"{\"x\":1}"}
        ]}))
        .unwrap();
        assert_eq!(parsed.items.len(), 2);
        assert_eq!(parsed.calls[0].arguments, json!({"x": 1}));
    }

    #[test]
    fn request_builders_are_stateless_and_wire_specific() {
        let responses = responses_request(
            &definition(WireApi::Responses),
            "system",
            &[responses_user_message("hello")],
            &[],
        );
        assert_eq!(responses["store"], false);
        assert_eq!(responses["reasoning"]["effort"], "high");
        assert!(responses.get("previous_response_id").is_none());
        let anthropic = anthropic_request(
            &definition(WireApi::AnthropicMessages),
            "system",
            &[anthropic_user_message("hello")],
            &[],
        );
        assert_eq!(anthropic["max_tokens"], MAX_TOKENS);
        assert_eq!(anthropic["output_config"]["effort"], "high");
        assert_eq!(anthropic["temperature"], 0.5);
    }

    #[test]
    fn provider_request_serialization_enforces_the_body_limit() {
        assert_eq!(
            serialize_provider_body(&json!({"ok": true})).unwrap(),
            "{\"ok\":true}"
        );
        let oversized = json!("x".repeat(MAX_PROVIDER_BODY_BYTES));
        let error = serialize_provider_body(&oversized).unwrap_err();
        assert_eq!(error.kind, "context_limit");
        assert!(error.message.len() <= ERROR_MESSAGE_LIMIT);
        assert!(!error.message.contains("xxxxxxxx"));
    }

    #[test]
    fn provider_context_limit_errors_are_safe_and_portable() {
        for (status, body) in [
            (StatusCode::PAYLOAD_TOO_LARGE, br#"secret payload"#.as_slice()),
            (
                StatusCode::BAD_REQUEST,
                br#"{"error":{"code":"context_length_exceeded","message":"secret conversation maximum context length"}}"#
                    .as_slice(),
            ),
        ] {
            let error = status_error(status, body);
            assert_eq!(error.kind, "context_limit");
            assert!(error.resumable);
            assert!(!error.message.contains("secret"));
            assert!(error.message.len() <= ERROR_MESSAGE_LIMIT);
        }
        assert_eq!(
            status_error(StatusCode::BAD_REQUEST, br#"{"error":{"code":"invalid"}}"#).kind,
            "provider_error"
        );
    }

    #[test]
    fn provider_response_accumulation_is_chunk_independent_and_bounded() {
        let mut body = Vec::new();
        append_provider_bytes(&mut body, b"one").unwrap();
        append_provider_bytes(&mut body, b"two").unwrap();
        assert_eq!(body, b"onetwo");

        let mut at_limit = vec![0; MAX_PROVIDER_BODY_BYTES - 1];
        append_provider_bytes(&mut at_limit, b"x").unwrap();
        assert_eq!(at_limit.len(), MAX_PROVIDER_BODY_BYTES);
        assert!(append_provider_bytes(&mut at_limit, b"x").is_err());
    }

    #[test]
    fn parses_anthropic_tool_use_and_text() {
        let parsed = parse_anthropic_response(json!({"stop_reason":"tool_use","content":[
            {"type":"thinking","thinking":"opaque","signature":"sig"},
            {"type":"tool_use","id":"tu_1","name":"tool","input":{"x":true}},
            {"type":"text","text":"working"}
        ]}))
        .unwrap();
        assert_eq!(parsed.content.len(), 3);
        assert_eq!(parsed.calls[0].id, "tu_1");
        assert_eq!(parsed.text.as_deref(), Some("working"));
    }

    #[test]
    fn anthropic_commits_all_tool_results_in_one_user_message() {
        let mut history = Vec::new();
        let results = anthropic_tool_results(vec![
            ToolResult {
                id: "one".into(),
                output: "first".into(),
            },
            ToolResult {
                id: "two".into(),
                output: "second".into(),
            },
        ]);
        commit_anthropic(
            &mut history,
            vec![json!({"type":"thinking", "signature":"opaque"})],
            results,
        );
        assert_eq!(history.len(), 2);
        assert_eq!(history[0]["role"], "assistant");
        assert_eq!(history[1]["role"], "user");
        assert_eq!(history[1]["content"].as_array().unwrap().len(), 2);
        assert_eq!(history[1]["content"][0]["tool_use_id"], "one");
        assert_eq!(history[1]["content"][1]["tool_use_id"], "two");
    }

    #[test]
    fn state_and_error_messages_are_safe() {
        let mut state = ConversationState::new(&WireApi::Responses);
        if let ConversationState::Responses(history) = &mut state {
            history.push(responses_user_message("hello"));
        }
        assert!(matches!(state, ConversationState::Responses(ref history) if history.len() == 1));
        assert_eq!(
            endpoint(&Url::parse("https://example.test/v1").unwrap(), "responses")
                .unwrap()
                .as_str(),
            "https://example.test/v1/responses"
        );
        assert!(
            bounded_message(&("x\n".to_owned() + &"y".repeat(500))).len() <= ERROR_MESSAGE_LIMIT
        );
    }

    #[test]
    fn activity_summaries_allowlist_and_bound_untrusted_arguments() {
        let workspace = Path::new("/workspace");
        let (summary, _, target) = safe_tool_activity(
            "shell\nSECRET",
            &json!({"command":"curl https://secret.invalid --token=secret"}),
            workspace,
        );
        assert_eq!(summary, "Running shell command");
        assert!(target.is_none());
        let (summary, _, target) = safe_tool_activity(
            "read_file",
            &json!({"path":"src/main.rs", "token":"secret", "body":"x".repeat(10_000)}),
            workspace,
        );
        assert_eq!(summary, "Reading src/main.rs");
        assert_eq!(target.as_deref(), Some("src/main.rs"));
        let (summary, _, target) =
            safe_tool_activity("shell", &json!({"command":"cargo test -p app"}), workspace);
        assert_eq!(summary, "Running cargo test");
        assert!(target.is_none());
        let (summary, _, _) = safe_tool_activity(
            "shell__execute_command",
            &json!({"command":["cargo", "clippy", "--token", "secret"]}),
            workspace,
        );
        assert_eq!(summary, "Running cargo clippy");

        // Absolute in-workspace paths are summarized relative to the workspace
        // root. `/workspace` (POSIX) is not an absolute path on Windows, so the
        // workspace root is built with the platform's separators.
        let workspace_root = if cfg!(windows) {
            r"C:\workspace".to_string()
        } else {
            "/workspace".to_string()
        };
        let workspace = Path::new(&workspace_root);
        let inside = |relative: &str| {
            if cfg!(windows) {
                format!(r"C:\workspace\{}", relative.replace('/', "\\"))
            } else {
                format!("/workspace/{relative}")
            }
        };
        for (tool, expected) in [
            ("filesystem__read_text_file", "Reading src/main.rs"),
            ("filesystem.write_file", "Writing src/main.rs"),
            ("filesystem__search_files", "Searching src/main.rs"),
        ] {
            let (summary, _, target) =
                safe_tool_activity(tool, &json!({"path": inside("src/main.rs")}), workspace);
            assert_eq!(summary, expected);
            assert_eq!(target.as_deref(), Some("src/main.rs"));
        }
        let outside = if cfg!(windows) {
            r"C:\outside\secret"
        } else {
            "/outside/secret"
        };
        for path in [outside, "../secret", &format!("{workspace_root}/../secret")] {
            let (summary, _, target) =
                safe_tool_activity("read_file", &json!({"path":path}), workspace);
            assert_eq!(summary, "Calling read_file");
            assert!(target.is_none());
        }
        let (summary, _, _) = safe_tool_activity(
            "shell",
            &json!({"command":"cargo test -- Authorization: Bearer token API_KEY=secret"}),
            workspace,
        );
        assert_eq!(summary, "Running cargo test");
        assert!(!summary.contains("token"));
        let (summary, _, target) = safe_tool_activity(
            "child/mcp",
            &json!({"arguments":"x".repeat(1_000_000), "token":"secret"}),
            workspace,
        );
        assert_eq!(summary, "Calling child/mcp");
        assert!(target.is_none());
        assert!(summary.len() < 120);
    }
}
