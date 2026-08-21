use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    future::Future,
    path::Path,
    sync::Arc,
};

use anyhow::{Result, anyhow, bail};
use rmcp::{
    ClientLifecycleMode, ClientServiceExt, RoleClient,
    model::{CallToolRequestParams, JsonObject, ProtocolVersion},
    service::RunningService,
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::Value;
use tokio::{process::Command, time::timeout};
use tokio_util::sync::CancellationToken;

use super::definition::{
    AgentDefinition, McpServerDefinition, PermissionMode, PermissionPolicy, SandboxMode,
};
use super::timeouts::{
    CHILD_MCP_CALL_TIMEOUT, CHILD_MCP_SHUTDOWN_TIMEOUT, CHILD_MCP_STARTUP_TIMEOUT,
    PROVIDER_CONNECT_TIMEOUT,
};
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_DESCRIPTION_BYTES: usize = 4096;
const INTERRUPTION_ERROR: &str = "child_mcp_interrupted";
const MAX_CHILD_SERVERS: usize = 16;
const MAX_CHILD_TOOLS: usize = 256;
const MAX_SCHEMA_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_SCHEMA_BYTES: usize = 4 * 1024 * 1024;

pub(crate) struct ChildTool {
    pub provider_name: String,
    pub description: String,
    pub input_schema: Arc<JsonObject>,
}

pub(crate) struct ChildMcpManager {
    connections: Vec<RunningService<RoleClient, ()>>,
    tools: Vec<ChildTool>,
    routes: BTreeMap<String, (usize, String)>,
}

impl ChildMcpManager {
    pub(crate) async fn connect(
        definition: &AgentDefinition,
        workspace: &Path,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        if definition.mcp_servers.len() > MAX_CHILD_SERVERS {
            bail!("too many configured child MCP servers");
        }
        if !workspace.is_dir() {
            bail!("child MCP workspace is not a directory");
        }
        let mut connections = Vec::new();
        let mut catalog = Vec::new();
        let mut retained_schema_bytes = 0;
        for (server_name, server) in &definition.mcp_servers {
            let connection = match connect_one(server, workspace, cancel).await {
                Ok(connection) => connection,
                Err(error) => {
                    close_all(&mut connections).await;
                    return Err(error.context("unable to connect child MCP server"));
                }
            };
            if connection
                .peer_info()
                .is_none_or(|info| info.protocol_version != ProtocolVersion::V_2026_07_28)
            {
                let mut connection = connection;
                let _ = connection
                    .close_with_timeout(CHILD_MCP_SHUTDOWN_TIMEOUT)
                    .await;
                close_all(&mut connections).await;
                bail!("child MCP server did not discover protocol 2026-07-28");
            }
            let index = connections.len();
            let listed = match wait_cancelled(cancel, connection.list_all_tools()).await {
                Ok(tools) => tools,
                Err(error) => {
                    let mut connection = connection;
                    let _ = connection
                        .close_with_timeout(CHILD_MCP_SHUTDOWN_TIMEOUT)
                        .await;
                    close_all(&mut connections).await;
                    return Err(error.context("unable to list child MCP tools"));
                }
            };
            if listed.len() > MAX_CHILD_TOOLS {
                let mut connection = connection;
                let _ = connection
                    .close_with_timeout(CHILD_MCP_SHUTDOWN_TIMEOUT)
                    .await;
                close_all(&mut connections).await;
                bail!("child MCP tool limit exceeded");
            }
            for tool in listed {
                let original = tool.name.to_string();
                let qualified = qualified_name(server_name, &original);
                if permitted(
                    &definition.permission,
                    &definition.sandbox,
                    &original,
                    &qualified,
                    tool.annotations.as_ref(),
                ) {
                    let schema_bytes = match schema_size(tool.input_schema.as_ref()) {
                        Ok(size) => size,
                        Err(_) => {
                            let mut connection = connection;
                            let _ = connection
                                .close_with_timeout(CHILD_MCP_SHUTDOWN_TIMEOUT)
                                .await;
                            close_all(&mut connections).await;
                            bail!("invalid child MCP tool schema");
                        }
                    };
                    if catalog_limits_exceeded(
                        catalog.len() + 1,
                        retained_schema_bytes,
                        schema_bytes,
                    ) {
                        let mut connection = connection;
                        let _ = connection
                            .close_with_timeout(CHILD_MCP_SHUTDOWN_TIMEOUT)
                            .await;
                        close_all(&mut connections).await;
                        bail!("child MCP tool or schema limit exceeded");
                    }
                    retained_schema_bytes += schema_bytes;
                    catalog.push((
                        qualified,
                        index,
                        original,
                        safe_description(
                            tool.description.as_deref().unwrap_or(""),
                            MAX_DESCRIPTION_BYTES,
                        ),
                        tool.input_schema,
                    ));
                }
            }
            connections.push(connection);
        }
        catalog.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.cmp(&b.2)).then(a.1.cmp(&b.1)));
        let mut used = BTreeSet::new();
        let mut tools = Vec::with_capacity(catalog.len());
        let mut routes = BTreeMap::new();
        for (base, index, original, description, input_schema) in catalog {
            let name = unique_name(base, &mut used);
            routes.insert(name.clone(), (index, original));
            tools.push(ChildTool {
                provider_name: name,
                description,
                input_schema,
            });
        }
        Ok(Self {
            connections,
            tools,
            routes,
        })
    }

    pub(crate) fn tools(&self) -> &[ChildTool] {
        &self.tools
    }

    pub(crate) async fn call(
        &self,
        provider_name: &str,
        arguments: Value,
        cancel: &CancellationToken,
    ) -> std::result::Result<String, ChildCallError> {
        let Some((index, original)) = self.routes.get(provider_name) else {
            return Err(ChildCallError::Failed);
        };
        let Value::Object(arguments) = arguments else {
            return Err(ChildCallError::Failed);
        };
        let request = CallToolRequestParams::new(original.clone()).with_arguments(arguments);
        let result = tokio::select! {
            _ = cancel.cancelled() => return Err(ChildCallError::Interrupted),
            result = timeout(CHILD_MCP_CALL_TIMEOUT, self.connections[*index].call_tool(request)) => result.map_err(|_| ChildCallError::TimedOut)?,
        }
        .map_err(|_| ChildCallError::Failed)?;
        Ok(render_output(
            &result.content,
            result.structured_content.as_ref(),
        ))
    }

    pub(crate) async fn shutdown(&mut self) {
        close_all(&mut self.connections).await;
    }
}

async fn connect_one(
    server: &McpServerDefinition,
    workspace: &Path,
    cancel: &CancellationToken,
) -> Result<RunningService<RoleClient, ()>> {
    let lifecycle = ClientLifecycleMode::Discover {
        preferred_versions: vec![ProtocolVersion::V_2026_07_28],
    };
    match server {
        McpServerDefinition::Stdio { command, args, env } => {
            let mut process = Command::new(command);
            process
                .args(args)
                .env_clear()
                .current_dir(workspace)
                .kill_on_drop(true)
                .stderr(std::process::Stdio::inherit());
            for name in [
                "PATH",
                "HOME",
                "USERPROFILE",
                "SYSTEMROOT",
                "PATHEXT",
                "TEMP",
                "TMP",
            ] {
                if let Some(value) = std::env::var_os(name) {
                    process.env(name, value);
                }
            }
            for (key, value) in env {
                process.env(key, interpolate(value)?);
            }
            let transport = rmcp::transport::TokioChildProcess::builder(process)
                .spawn()?
                .0;
            startup(cancel, ().serve_with_lifecycle(transport, lifecycle)).await
        }
        McpServerDefinition::Http { url, headers } => {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let client = reqwest::Client::builder()
                .connect_timeout(PROVIDER_CONNECT_TIMEOUT)
                .timeout(CHILD_MCP_CALL_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()?;
            let mut config = http_config(url.as_str());
            let mut custom_headers = HashMap::new();
            for (name, value) in headers {
                let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| anyhow!("invalid child MCP HTTP header name"))?;
                let value = reqwest::header::HeaderValue::from_str(&interpolate(value)?)
                    .map_err(|_| anyhow!("invalid child MCP HTTP header value"))?;
                custom_headers.insert(name, value);
            }
            config.custom_headers = custom_headers;
            let transport = StreamableHttpClientTransport::with_client(client, config);
            startup(cancel, ().serve_with_lifecycle(transport, lifecycle)).await
        }
    }
}

async fn startup<F>(cancel: &CancellationToken, future: F) -> Result<RunningService<RoleClient, ()>>
where
    F: Future<
        Output = std::result::Result<
            RunningService<RoleClient, ()>,
            rmcp::service::ClientInitializeError,
        >,
    >,
{
    tokio::select! {
        _ = cancel.cancelled() => bail!(INTERRUPTION_ERROR),
        result = timeout(CHILD_MCP_STARTUP_TIMEOUT, future) => result.map_err(|_| anyhow!("child MCP startup timed out"))?.map_err(|_| anyhow!("child MCP startup failed")),
    }
}

async fn wait_cancelled<T, F>(cancel: &CancellationToken, future: F) -> Result<T>
where
    F: Future<Output = std::result::Result<T, rmcp::service::ServiceError>>,
{
    tokio::select! { _ = cancel.cancelled() => bail!(INTERRUPTION_ERROR), result = timeout(CHILD_MCP_CALL_TIMEOUT, future) => result.map_err(|_| anyhow!("child MCP request timed out"))?.map_err(|_| anyhow!("child MCP request failed")), }
}

async fn close_all(connections: &mut [RunningService<RoleClient, ()>]) {
    let _ = timeout(CHILD_MCP_SHUTDOWN_TIMEOUT, async {
        for connection in connections {
            let _ = connection
                .close_with_timeout(CHILD_MCP_SHUTDOWN_TIMEOUT)
                .await;
        }
    })
    .await;
}

fn interpolate(value: &str) -> Result<String> {
    interpolate_with(value, |name| std::env::var(name).ok())
}

fn interpolate_with<F>(value: &str, mut resolve: F) -> Result<String>
where
    F: FnMut(&str) -> Option<String>,
{
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            bail!("malformed environment placeholder")
        };
        let name = &after[..end];
        if !valid_env_name(name) {
            bail!("malformed environment placeholder")
        }
        let resolved =
            resolve(name).ok_or_else(|| anyhow!("missing environment placeholder {name}"))?;
        output.push_str(&resolved);
        rest = &after[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.bytes();
    matches!(chars.next(), Some(b) if b.is_ascii_alphabetic() || b == b'_')
        && chars.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn qualified_name(server: &str, tool: &str) -> String {
    let mut server = sanitize(server);
    let mut tool = sanitize(tool);
    if server.len() + 2 + tool.len() > 64 {
        server.truncate(31);
        tool.truncate(31);
    }
    format!("{server}__{tool}")
}
fn sanitize(value: &str) -> String {
    let mut out: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out.push('_');
    }
    out.truncate(64);
    out
}
fn unique_name(base: String, used: &mut BTreeSet<String>) -> String {
    if used.insert(base.clone()) {
        return base;
    }
    for n in 2.. {
        let suffix = format!("_{n}");
        let mut candidate = base.clone();
        candidate.truncate(64 - suffix.len());
        candidate.push_str(&suffix);
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!()
}
fn http_config(url: &str) -> StreamableHttpClientTransportConfig {
    let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_owned());
    config.reinit_on_expired_session = false;
    config
}
fn schema_size(schema: &JsonObject) -> Result<usize> {
    serde_json::to_vec(schema)
        .map(|serialized| serialized.len())
        .map_err(|_| anyhow!("invalid child MCP tool schema"))
}
fn catalog_limits_exceeded(
    tool_count: usize,
    retained_schema_bytes: usize,
    schema_bytes: usize,
) -> bool {
    tool_count > MAX_CHILD_TOOLS
        || schema_bytes > MAX_SCHEMA_BYTES
        || retained_schema_bytes.saturating_add(schema_bytes) > MAX_TOTAL_SCHEMA_BYTES
}
fn permitted(
    policy: &PermissionPolicy,
    sandbox: &SandboxMode,
    original: &str,
    qualified: &str,
    annotations: Option<&rmcp::model::ToolAnnotations>,
) -> bool {
    let matches = |set: &BTreeSet<String>| set.contains(original) || set.contains(qualified);
    let configured = policy.allowed_mcp_tools.as_ref().is_none_or(matches)
        && !matches(&policy.disallowed_mcp_tools);
    let read_only =
        matches!(policy.mode, PermissionMode::ReadOnly) || matches!(sandbox, SandboxMode::ReadOnly);
    configured
        && (!read_only
            || annotations.is_some_and(|annotations| {
                annotations.read_only_hint == Some(true)
                    && annotations.destructive_hint != Some(true)
            }))
}
fn bounded_text(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    if limit < '…'.len_utf8() {
        return String::new();
    }
    let mut end = limit;
    while !value.is_char_boundary(end - '…'.len_utf8()) {
        end -= 1;
    }
    format!("{}…", &value[..end - '…'.len_utf8()])
}
fn safe_description(value: &str, limit: usize) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    bounded_text(&cleaned, limit)
}
fn render_output(content: &[rmcp::model::ContentBlock], structured: Option<&Value>) -> String {
    let value = serde_json::json!({ "content": content, "structuredContent": structured });
    let serialized = serde_json::to_string(&value)
        .unwrap_or_else(|_| "{\"content\":[],\"structuredContent\":null}".into());
    bounded_text(&serialized, MAX_OUTPUT_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interpolation_is_strict() {
        assert_eq!(
            interpolate_with("x${CHILD_MCP_TEST}/${CHILD_MCP_TEST}", |name| {
                (name == "CHILD_MCP_TEST").then(|| "one".to_owned())
            })
            .unwrap(),
            "xone/one"
        );
        assert_eq!(
            interpolate_with("plain text", |_| None).unwrap(),
            "plain text"
        );
        assert!(interpolate_with("${CHILD_MCP_ABSENT}", |_| None).is_err());
        assert!(interpolate_with("${bad-name}", |_| None).is_err());
        assert!(interpolate_with("${CHILD_MCP_TEST", |_| None).is_err());
    }
    #[test]
    fn names_are_safe_and_unique() {
        let mut used = BTreeSet::new();
        assert_eq!(qualified_name("a b", "x/y"), "a_b__x_y");
        assert_eq!(unique_name("same".into(), &mut used), "same");
        assert_eq!(unique_name("same".into(), &mut used), "same_2");
        let base = qualified_name(&"server".repeat(20), &"tool".repeat(20));
        let first = unique_name(base.clone(), &mut used);
        let second = unique_name(base, &mut used);
        for name in [&first, &second] {
            assert!(name.len() <= 64);
            assert!(
                name.bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
            );
        }
        assert!(second.ends_with("_2"));
    }
    #[test]
    fn http_configuration_disables_session_reinitialization() {
        assert!(!http_config("https://example.test/mcp").reinit_on_expired_session);
    }
    #[test]
    fn permission_checks_both_names() {
        let mut allowed = BTreeSet::new();
        allowed.insert("server__tool".into());
        let policy = PermissionPolicy {
            mode: super::super::definition::PermissionMode::Default,
            allowed_mcp_tools: Some(allowed),
            disallowed_mcp_tools: BTreeSet::new(),
        };
        assert!(permitted(
            &policy,
            &SandboxMode::Default,
            "tool",
            "server__tool",
            None,
        ));
    }
    #[test]
    fn read_only_policy_requires_explicit_safe_tool_annotation() {
        use rmcp::model::{Tool, ToolAnnotations};

        let safe = Tool::new("tool", "", JsonObject::new())
            .with_annotations(ToolAnnotations::new().read_only(true).destructive(false));
        let destructive = Tool::new("tool", "", JsonObject::new())
            .with_annotations(ToolAnnotations::new().read_only(true).destructive(true));
        let missing_read_only = Tool::new("tool", "", JsonObject::new())
            .with_annotations(ToolAnnotations::new().destructive(false));
        let policy = PermissionPolicy {
            mode: PermissionMode::ReadOnly,
            allowed_mcp_tools: None,
            disallowed_mcp_tools: BTreeSet::new(),
        };
        for annotations in [
            None,
            missing_read_only.annotations.as_ref(),
            destructive.annotations.as_ref(),
        ] {
            assert!(!permitted(
                &policy,
                &SandboxMode::Default,
                "tool",
                "server__tool",
                annotations,
            ));
        }
        assert!(permitted(
            &policy,
            &SandboxMode::Default,
            "tool",
            "server__tool",
            safe.annotations.as_ref(),
        ));
        let default_policy = PermissionPolicy {
            mode: PermissionMode::Default,
            allowed_mcp_tools: None,
            disallowed_mcp_tools: BTreeSet::new(),
        };
        assert!(!permitted(
            &default_policy,
            &SandboxMode::ReadOnly,
            "tool",
            "server__tool",
            None,
        ));
        assert!(permitted(
            &default_policy,
            &SandboxMode::ReadOnly,
            "tool",
            "server__tool",
            safe.annotations.as_ref(),
        ));
    }
    #[test]
    fn output_is_bounded() {
        let text = bounded_text(&"x".repeat(MAX_OUTPUT_BYTES + 1), MAX_OUTPUT_BYTES);
        assert!(text.len() <= MAX_OUTPUT_BYTES);
    }
    #[test]
    fn schema_and_catalog_limits_fail_closed() {
        let schema = JsonObject::new();
        assert_eq!(schema_size(&schema).unwrap(), 2);
        assert!(!catalog_limits_exceeded(1, 0, 2));
        assert!(catalog_limits_exceeded(MAX_CHILD_TOOLS + 1, 0, 2));
        assert!(catalog_limits_exceeded(1, 0, MAX_SCHEMA_BYTES + 1));
        assert!(catalog_limits_exceeded(1, MAX_TOTAL_SCHEMA_BYTES - 1, 2,));
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChildCallError {
    Interrupted,
    TimedOut,
    Failed,
}
