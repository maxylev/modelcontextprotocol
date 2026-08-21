mod activity;
mod child_mcp;
mod definition;
mod discovery;
mod markdown;
mod provider;
mod runtime;
mod timeouts;
mod toml;

use std::{future, path::PathBuf, sync::Arc, time::Duration};

use rmcp::{
    ErrorData as McpError, ServerHandler,
    model::{
        CallToolRequestMethod, CallToolRequestParams, CallToolResponse, CallToolResult,
        ContentBlock, Implementation, ListToolsResult, NotificationMetaObject,
        ProgressNotificationParam, ServerCapabilities, ServerInfo, Tool, ToolAnnotations,
    },
};
use serde::Deserialize;
use serde_json::{Value, json};

use self::activity::bound;
use self::runtime::{AgentRuntime, InputAck, RuntimeError, SpawnResult, WaitResult};
use self::timeouts::MAX_WAIT_AGENT_TIMEOUT_MS;

const PROGRESS_QUEUE_CAPACITY: usize = 16;
const PROGRESS_NOTIFICATION_TIMEOUT: Duration = Duration::from_millis(100);

fn progress_notification(
    token: rmcp::model::ProgressToken,
    progress: f64,
    agent: &runtime::AgentResult,
    remaining: u64,
) -> ProgressNotificationParam {
    let summary = match agent.state {
        runtime::AgentState::Completed => "Completed".into(),
        runtime::AgentState::Failed => "Failed".into(),
        runtime::AgentState::Running => agent
            .activity
            .as_ref()
            .map(|a| a.summary.clone())
            .unwrap_or_else(|| "Working".into()),
    };
    let name = bound(agent.name.clone().unwrap_or_else(|| agent.id.clone()), 120);
    let mut meta = NotificationMetaObject::new();
    meta.insert(
        "io.modelcontextprotocol/agents".into(),
        json!({
            "agent": {
                "agent_id": agent.id,
                "name": agent.name,
                "status": agent.state,
                "activity": agent.activity,
                "total_elapsed_ms": agent.total_elapsed_ms,
            },
            "wait_timeout_remaining_ms": remaining,
        }),
    );
    let mut notification = ProgressNotificationParam::new(token, progress)
        .with_message(bound(format!("{name} · {summary}"), 256));
    notification.meta = Some(meta);
    notification
}

pub(crate) struct AgentsServer {
    runtime: Arc<AgentRuntime>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnArgs {
    name: String,
    task: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputArgs {
    target: String,
    message: String,
    #[serde(default)]
    interrupt: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    targets: Vec<String>,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
}
fn default_timeout() -> u64 {
    30_000
}

impl AgentsServer {
    pub(crate) fn new(workspace: PathBuf) -> Result<Self, RuntimeError> {
        Ok(Self {
            runtime: Arc::new(AgentRuntime::new(workspace)?),
        })
    }
    fn tools(&self) -> Vec<Tool> {
        if self.runtime.registry().is_empty() {
            vec![]
        } else {
            vec![self.spawn_tool(), self.input_tool(), self.wait_tool()]
        }
    }
    fn spawn_tool(&self) -> Tool {
        let catalog = self
            .runtime
            .registry()
            .catalog()
            .iter()
            .map(|a| format!("- {}: {}", a.name, a.description))
            .collect::<Vec<_>>()
            .join("\n");
        let names = self.runtime.registry().names();
        let schema=json!({"type":"object","properties":{"name":{"type":"string","enum":names},"task":{"type":"string","minLength":1}},"required":["name","task"],"additionalProperties":false}).as_object().unwrap().clone();
        Tool::new(
            "spawn_agent",
            format!("Spawn a local workspace agent to run a task in the background. This returns promptly before the agent completes; save the returned agent_id and use wait_agent to collect the result. Calls may run in parallel up to the runtime capacity. Available agents:\n{catalog}"),
            schema,
        )
        .with_output_schema::<SpawnResult>()
        .with_annotations(
            ToolAnnotations::new()
                .read_only(false)
                .destructive(true)
                .idempotent(false)
                .open_world(true),
        )
    }
    fn input_tool(&self) -> Tool {
        let schema=json!({"type":"object","properties":{"target":{"type":"string","minLength":1},"message":{"type":"string","minLength":1},"interrupt":{"type":"boolean","default":false}},"required":["target","message"],"additionalProperties":false}).as_object().unwrap().clone();
        Tool::new(
            "send_input",
            "Send follow-up input to an agent session. Set interrupt to true to cancel its active run cooperatively and continue the same session with this input.",
            schema,
        )
        .with_output_schema::<InputAck>()
        .with_annotations(
            ToolAnnotations::new()
                .read_only(false)
                .destructive(true)
                .idempotent(false)
                .open_world(true),
        )
    }
    fn wait_tool(&self) -> Tool {
        let schema=json!({"type":"object","properties":{"targets":{"type":"array","items":{"type":"string","minLength":1},"minItems":1,"uniqueItems":true},"timeout_ms":{"type":"integer","minimum":0,"maximum":MAX_WAIT_AGENT_TIMEOUT_MS,"default":30000}},"required":["targets"],"additionalProperties":false}).as_object().unwrap().clone();
        Tool::new("wait_agent", "Wait until all requested agents reach a terminal state or the timeout expires, whichever happens first. timeout_ms is a maximum wait duration, not a sleep interval. Already-finished agents return immediately. Calling this tool pauses the parent model until the tool returns.", schema).with_output_schema::<WaitResult>().with_annotations(ToolAnnotations::new().read_only(true).destructive(false).open_world(false))
    }
    async fn call(
        &self,
        name: &str,
        arguments: Option<rmcp::model::JsonObject>,
        context: Option<rmcp::service::RequestContext<rmcp::RoleServer>>,
    ) -> CallToolResult {
        let parsed = arguments
            .map(Value::Object)
            .ok_or_else(|| RuntimeError::new("invalid_request", "missing arguments"));
        let result: Result<Value, RuntimeError> = match name {
            "spawn_agent" => match parsed.and_then(|v| {
                serde_json::from_value::<SpawnArgs>(v)
                    .map_err(|_| RuntimeError::new("invalid_request", "invalid arguments"))
            }) {
                Ok(a) => self.runtime.spawn(&a.name, &a.task).await.and_then(|v| {
                    serde_json::to_value(v).map_err(|_| {
                        RuntimeError::new("runtime_error", "unable to serialize response")
                    })
                }),
                Err(e) => Err(e),
            },
            "send_input" => match parsed.and_then(|v| {
                serde_json::from_value::<InputArgs>(v)
                    .map_err(|_| RuntimeError::new("invalid_request", "invalid arguments"))
            }) {
                Ok(a) => self
                    .runtime
                    .send_input(&a.target, &a.message, a.interrupt)
                    .await
                    .and_then(|v| {
                        serde_json::to_value(v).map_err(|_| {
                            RuntimeError::new("runtime_error", "unable to serialize response")
                        })
                    }),
                Err(e) => Err(e),
            },
            "wait_agent" => match parsed.and_then(|v| {
                serde_json::from_value::<WaitArgs>(v)
                    .map_err(|_| RuntimeError::new("invalid_request", "invalid arguments"))
            }) {
                Ok(a) => self.wait_with_progress(a, context).await.and_then(|v| {
                    serde_json::to_value(v).map_err(|_| {
                        RuntimeError::new("runtime_error", "unable to serialize response")
                    })
                }),
                Err(e) => Err(e),
            },
            _ => Err(RuntimeError::new("unknown_tool", "unknown tool")),
        };
        match result {
            Ok(value) => {
                let mut out = CallToolResult::structured(value);
                out.content
                    .push(ContentBlock::text("Agent request accepted."));
                out
            }
            Err(e) => CallToolResult::error(vec![ContentBlock::text(
                serde_json::to_string(&e).expect("error serializes"),
            )]),
        }
    }
    async fn wait_with_progress(
        &self,
        args: WaitArgs,
        context: Option<rmcp::service::RequestContext<rmcp::RoleServer>>,
    ) -> Result<WaitResult, RuntimeError> {
        let Some(context) = context else {
            return self.runtime.wait(&args.targets, args.timeout_ms).await;
        };
        let Some(token) = context.meta.get_progress_token() else {
            return self.runtime.wait(&args.targets, args.timeout_ms).await;
        };
        let (updates, mut receiver) = tokio::sync::mpsc::channel(PROGRESS_QUEUE_CAPACITY);
        let runtime = self.runtime.clone();
        let targets = args.targets.clone();
        let worker = tokio::spawn(async move {
            runtime
                .wait_observing(&targets, args.timeout_ms, updates)
                .await
        });
        let mut progress = 0.0;
        while let Some(update) = receiver.recv().await {
            for agent in &update.result.agents {
                progress += 1.0;
                let notification = progress_notification(
                    token.clone(),
                    progress,
                    agent,
                    update.wait_timeout_remaining_ms,
                );
                if !matches!(
                    tokio::time::timeout(
                        PROGRESS_NOTIFICATION_TIMEOUT,
                        context.peer.notify_progress(notification),
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
        }
        worker
            .await
            .map_err(|_| RuntimeError::new("runtime_error", "agent wait task failed"))?
    }
}

impl ServerHandler for AgentsServer {
    fn initialize(
        &self,
        _request: rmcp::model::InitializeRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<rmcp::model::InitializeResult, McpError>>
    + rmcp::service::MaybeSendFuture
    + '_ {
        crate::support::reject_legacy_initialize()
    }

    fn supported_protocol_versions(
        &self,
    ) -> std::borrow::Cow<'static, [rmcp::model::ProtocolVersion]> {
        std::borrow::Cow::Borrowed(crate::support::SUPPORTED_PROTOCOL_VERSIONS)
    }

    fn get_info(&self) -> ServerInfo {
        let capabilities = if self.runtime.registry().is_empty() {
            ServerCapabilities::builder().build()
        } else {
            ServerCapabilities::builder().enable_tools().build()
        };
        ServerInfo::new(capabilities)
            .with_server_info(Implementation::new("mcp-agents", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Spawn local workspace agents for background tasks, save each returned agent_id, send follow-up input when needed, and call wait_agent to collect terminal results.",
            )
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools().into_iter().find(|t| t.name == name)
    }
    fn list_tools(
        &self,
        _: Option<rmcp::model::PaginatedRequestParams>,
        _: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpError>>
    + rmcp::service::MaybeSendFuture
    + '_ {
        future::ready(Ok(ListToolsResult::with_all_items(self.tools())
            .with_ttl_ms(0)
            .with_cache_scope(rmcp::model::CacheScope::Private)))
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        if self.get_tool(&request.name).is_none() {
            Err(McpError::method_not_found::<CallToolRequestMethod>())
        } else {
            Ok(self
                .call(&request.name, request.arguments, Some(context))
                .await
                .into())
        }
    }
}

pub(crate) async fn run(workspace: PathBuf) -> anyhow::Result<()> {
    use rmcp::{ServiceExt, transport::stdio};
    let server = AgentsServer::new(workspace).map_err(|e| anyhow::anyhow!(e.message))?;
    let runtime = server.runtime.clone();
    let service = server
        .serve(stdio())
        .await
        .inspect_err(|e| tracing::error!("serving error: {e:?}"))?;
    service.waiting().await?;
    runtime.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{NumberOrString, ProgressToken};
    use std::fs;
    use tempfile::tempdir;

    fn server() -> AgentsServer {
        let root = tempdir().unwrap().keep();
        let agents = root.join(".agents/agents");
        fs::create_dir_all(&agents).unwrap();
        fs::write(
            agents.join("a.md"),
            "---\nname: alpha\ndescription: First\nmodel: g\nmodelProvider: openai\n---\nWork.",
        )
        .unwrap();
        fs::write(
            agents.join("b.md"),
            "---\nname: beta\ndescription: Second\nmodel: g\nmodelProvider: openai\n---\nWork.",
        )
        .unwrap();
        AgentsServer::new(root).unwrap()
    }

    #[test]
    fn tools_have_dynamic_catalog_schema_order_and_identity() {
        let server = server();
        assert_eq!(
            server
                .tools()
                .iter()
                .map(|tool| tool.name.as_ref())
                .collect::<Vec<_>>(),
            ["spawn_agent", "send_input", "wait_agent"]
        );
        let spawn = server.spawn_tool();
        let schema = serde_json::to_value(&spawn).unwrap();
        assert_eq!(schema["inputSchema"]["additionalProperties"], false);
        assert_eq!(
            schema["inputSchema"]["properties"]["name"]["enum"],
            json!(["alpha", "beta"])
        );
        assert!(
            spawn
                .description
                .as_deref()
                .unwrap()
                .contains("- alpha: First\n- beta: Second")
        );
        assert_eq!(
            serde_json::to_value(server.get_info()).unwrap()["serverInfo"]["name"],
            "mcp-agents"
        );
        assert_eq!(
            spawn.annotations.as_ref().unwrap().destructive_hint,
            Some(true)
        );
        assert_eq!(
            server
                .input_tool()
                .annotations
                .as_ref()
                .unwrap()
                .destructive_hint,
            Some(true)
        );
    }

    #[test]
    fn progress_metadata_is_namespaced_bounded_and_monotonic() {
        let running = runtime::AgentResult {
            id: "agt_1".into(),
            name: Some("agent\n🦀".repeat(100)),
            state: runtime::AgentState::Running,
            result: None,
            error: None,
            total_elapsed_ms: 12,
            activity: Some(
                activity::AgentActivity::new(activity::AgentActivityEvent::new(
                    activity::ActivityPhase::Model,
                    "Working\n".repeat(100),
                ))
                .snapshot(std::time::Instant::now()),
            ),
        };
        let token = ProgressToken(NumberOrString::String("wait-token".into()));
        let first = progress_notification(token.clone(), 1.0, &running, 999);
        let second = progress_notification(token.clone(), 2.0, &running, 998);
        assert_eq!(first.progress_token, token);
        assert!(first.progress < second.progress);
        assert!(first.message.as_deref().unwrap().len() <= 256);
        let metadata = first.meta.unwrap();
        assert_eq!(
            metadata["io.modelcontextprotocol/agents"]["wait_timeout_remaining_ms"],
            999
        );
        assert_eq!(
            metadata["io.modelcontextprotocol/agents"]["agent"]["activity"]["phase"],
            "model"
        );
        let terminal = runtime::AgentResult {
            state: runtime::AgentState::Completed,
            activity: None,
            result: Some(format!("large-result-marker{}", "x".repeat(1024 * 1024))),
            error: Some(runtime::RuntimeError::new(
                "provider_error",
                "full-error-marker",
            )),
            ..running
        };
        let terminal = progress_notification(token, 3.0, &terminal, 0);
        assert!(terminal.message.unwrap().contains("Completed"));
        let metadata = terminal.meta.unwrap();
        let agent = &metadata["io.modelcontextprotocol/agents"]["agent"];
        assert!(agent.get("result").is_none());
        assert!(agent.get("error").is_none());
        let rendered = serde_json::to_string(&metadata).unwrap();
        assert!(!rendered.contains("large-result-marker"));
        assert!(!rendered.contains("full-error-marker"));
        assert!(rendered.len() < 2048);
    }
}
