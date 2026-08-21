//! Real OpenRouter Responses client used by the skills + agents acceptance
//! test, plus the sanitized tool-call trace and `.env.test` loading.
//!
//! The only real network model used here is `openai/gpt-5.6-luna`. Nothing
//! here logs, prints, or stores the API key. The trace sanitizer redacts the
//! key substring from any retained argument before it is stored.

use std::path::Path;
use std::time::Duration;

use rmcp::model::Tool;
use serde_json::{Map, Value, json};

pub const MODEL: &str = "openai/gpt-5.6-luna";
pub const ENDPOINT: &str = "https://openrouter.ai/api/v1/responses";
pub const BASE_URL: &str = "https://openrouter.ai/api/v1";
pub const ENV_API_KEY: &str = "OPENROUTER_API_KEY";

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Upper bound on parent orchestration turns.
pub const MAX_PARENT_TURNS: usize = 25;
/// Tool results fed back to the parent model are capped in characters.
pub const TOOL_OUTPUT_CAP_CHARS: usize = 12_000;

// ---------------------------------------------------------------------------
// .env.test loading
// ---------------------------------------------------------------------------

/// Load `OPENROUTER_API_KEY` from the repository-root `.env.test` file. The
/// value stays in memory only and is never printed or Debug-formatted.
pub fn load_api_key_from_env_test() -> Result<String, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".env.test");
    let body = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    for raw in body.lines() {
        let line = raw.trim();
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != ENV_API_KEY {
            continue;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        if value.is_empty() {
            return Err(format!("{ENV_API_KEY} is present in .env.test but empty"));
        }
        return Ok(value.to_string());
    }
    Err(format!(
        "missing {ENV_API_KEY} in {}; source .env.test before running the ignored acceptance test",
        path.display()
    ))
}

// ---------------------------------------------------------------------------
// Responses wire model
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct FunctionCall {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug)]
pub struct ParsedResponse {
    /// The full `output` array, verbatim (reasoning/message/function_call).
    pub items: Vec<Value>,
    pub calls: Vec<FunctionCall>,
    /// Aggregated visible text (top-level `output_text` or message content).
    pub text: Option<String>,
}

pub struct ResponsesClient {
    http: reqwest::Client,
    api_key: String,
}

impl ResponsesClient {
    pub fn new(api_key: String) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("build responses client");
        Self { http, api_key }
    }

    /// One bounded OpenAI-Responses generation request.
    pub async fn complete(
        &self,
        model: &str,
        instructions: &str,
        input: &[Value],
        tools: &[Value],
    ) -> Result<ParsedResponse, String> {
        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(model.to_string()));
        body.insert(
            "instructions".to_string(),
            Value::String(instructions.to_string()),
        );
        body.insert("input".to_string(), Value::Array(input.to_vec()));
        body.insert("tools".to_string(), Value::Array(tools.to_vec()));
        body.insert("tool_choice".to_string(), Value::String("auto".into()));
        body.insert("parallel_tool_calls".to_string(), Value::Bool(true));
        body.insert("store".to_string(), Value::Bool(false));
        body.insert("reasoning".to_string(), json!({"effort": "medium"}));

        let request = self
            .http
            .post(ENDPOINT)
            .bearer_auth(&self.api_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_string(&Value::Object(body)).map_err(|e| e.to_string())?);
        let response = request
            .send()
            .await
            .map_err(|error| format!("provider transport error: {error}"))?;
        let status = response.status();
        if let Some(length) = response.content_length()
            && length as usize > MAX_BODY_BYTES
        {
            return Err(format!(
                "provider response body exceeds the {} byte bound (status {status})",
                MAX_BODY_BYTES
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| format!("provider body read error: {error}"))?;
        if bytes.len() > MAX_BODY_BYTES {
            return Err(format!(
                "provider response body exceeds the {} byte bound (status {status})",
                MAX_BODY_BYTES
            ));
        }
        if !status.is_success() {
            // Never include the response body in the error text: it may echo
            // request content and could contain sensitive material.
            return Err(format!("provider returned HTTP status {}", status.as_u16()));
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("provider returned malformed JSON: {error}"))?;
        parse_responses(&value)
    }
}

fn parse_responses(value: &Value) -> Result<ParsedResponse, String> {
    let output = value
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| "provider response omitted output".to_string())?;
    let mut calls = Vec::new();
    let mut block_text = String::new();
    for item in &output {
        if item.get("type").and_then(Value::as_str) == Some("function_call") {
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "function_call without call_id".to_string())?
                .to_string();
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "function_call without name".to_string())?
                .to_string();
            let raw_arguments = item
                .get("arguments")
                .and_then(Value::as_str)
                .ok_or_else(|| "function_call without arguments".to_string())?;
            let arguments: Value = serde_json::from_str(raw_arguments)
                .map_err(|error| format!("function_call arguments are not JSON: {error}"))?;
            calls.push(FunctionCall {
                call_id,
                name,
                arguments,
            });
        } else if item.get("type").and_then(Value::as_str) == Some("message")
            && let Some(content) = item.get("content").and_then(Value::as_array)
        {
            for block in content {
                if block.get("type").and_then(Value::as_str) == Some("output_text")
                    && let Some(text) = block.get("text").and_then(Value::as_str)
                {
                    block_text.push_str(text);
                }
            }
        }
    }
    let text = value
        .get("output_text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| (!block_text.is_empty()).then_some(block_text));
    Ok(ParsedResponse {
        items: output,
        calls,
        text,
    })
}

// ---------------------------------------------------------------------------
// Tool schema resolution for the provider
// ---------------------------------------------------------------------------

/// Convert a runtime MCP `Tool` into the Responses `tools` entry with the
/// schema's `$defs`/`$ref`/`allOf` resolved into a flat object.
pub fn provider_tool(tool: &Tool) -> Value {
    let schema = resolve_schema(&tool.schema_as_json_value());
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description.as_deref().unwrap_or(""),
        "parameters": schema,
    })
}

fn resolve_schema(root: &Value) -> Value {
    fn resolve(value: &Value, defs: &Map<String, Value>) -> Value {
        match value {
            Value::Object(map) => {
                let mut out = Map::new();
                if let Some(stripped) = map
                    .get("$ref")
                    .and_then(Value::as_str)
                    .and_then(|name| name.strip_prefix("#/$defs/"))
                    && let Some(def) = defs.get(stripped)
                {
                    out = def.as_object().cloned().unwrap_or_default();
                }
                if let Some(all_of) = map.get("allOf").and_then(Value::as_array) {
                    for item in all_of {
                        match resolve(item, defs) {
                            Value::Object(part) => {
                                for (key, value) in part {
                                    out.entry(key).or_insert(value);
                                }
                            }
                            value => {
                                out.insert("const".into(), value);
                            }
                        }
                    }
                }
                for (key, value) in map {
                    if key == "allOf" || key == "$ref" || key == "$defs" {
                        continue;
                    }
                    out.insert(key.clone(), resolve(value, defs));
                }
                Value::Object(out)
            }
            Value::Array(items) => {
                Value::Array(items.iter().map(|item| resolve(item, defs)).collect())
            }
            other => other.clone(),
        }
    }
    let defs = root
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    resolve(root, &defs)
}

// ---------------------------------------------------------------------------
// Sanitized trace
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum TraceResultKind {
    Ok,
    Error,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct TraceEntry {
    pub turn: usize,
    pub tool_name: String,
    /// Secret-redacted, bounded representation of the arguments.
    pub arguments: Value,
    pub result_kind: TraceResultKind,
}

#[derive(Default, Debug)]
pub struct Trace {
    pub entries: Vec<TraceEntry>,
}

impl Trace {
    pub fn record(
        &mut self,
        turn: usize,
        tool_name: &str,
        arguments: &Value,
        result_kind: TraceResultKind,
        secret: Option<&str>,
    ) {
        self.entries.push(TraceEntry {
            turn,
            tool_name: tool_name.to_string(),
            arguments: sanitize(arguments, secret),
            result_kind,
        });
    }

    pub fn calls(&self, tool_name: &str) -> Vec<&TraceEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.tool_name == tool_name)
            .collect()
    }

    /// The turn index of the first call to `tool_name`, if any.
    pub fn first_turn(&self, tool_name: &str) -> Option<usize> {
        self.calls(tool_name).first().map(|entry| entry.turn)
    }

    pub fn serialize_sanitized(&self, secret: Option<&str>) -> String {
        let serialized = serde_json::to_string(&self.entries).unwrap_or_default();
        match secret {
            // Only a non-empty secret triggers redaction. `None` and `Some("")`
            // return the serialized trace unchanged; replacing the empty
            // substring would balloon/garble the diagnostic.
            Some(secret) if !secret.is_empty() => serialized.replace(secret, "[REDACTED]"),
            _ => serialized,
        }
    }
}

/// Recursively bounds strings and redacts the secret substring so a trace can
/// never retain the provider token or unbounded tool output.
pub fn sanitize(value: &Value, secret: Option<&str>) -> Value {
    const MAX_STRING_CHARS: usize = 2000;
    match value {
        Value::String(text) => {
            let mut text = text.clone();
            if let Some(secret) = secret
                && !secret.is_empty()
                && text.contains(secret)
            {
                text = text.replace(secret, "[REDACTED]");
            }
            if text.chars().count() > MAX_STRING_CHARS {
                text = format!(
                    "{}…",
                    text.chars().take(MAX_STRING_CHARS).collect::<String>()
                );
            }
            Value::String(text)
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| sanitize(item, secret)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), sanitize(value, secret)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Quick assertion helper for semantic containment.
pub fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    let lower = haystack.to_ascii_lowercase();
    needles
        .iter()
        .any(|needle| lower.contains(&needle.to_ascii_lowercase()))
}

pub fn contains_all(haystack: &str, needles: &[&str]) -> bool {
    let lower = haystack.to_ascii_lowercase();
    needles
        .iter()
        .all(|needle| lower.contains(&needle.to_ascii_lowercase()))
}
