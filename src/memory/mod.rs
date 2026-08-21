mod graph;

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, Implementation, ListResourcesResult, PaginatedRequestParams,
        ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
        ResourceContents, ResourceUpdatedNotification, ResourceUpdatedNotificationParam,
        ServerCapabilities, ServerInfo, ServerNotification, SubscriptionFilter,
    },
    schemars,
    service::{RequestContext, SubscriptionContext},
    tool, tool_handler, tool_router,
};
use serde::Deserialize;
use tokio::sync::broadcast;

use crate::support::{SPEC_VERSION, tool_error};

use self::graph::{
    AddedObservation, Entity, KnowledgeGraph, KnowledgeGraphManager, ObservationInput, Relation,
};

pub const RESOURCE_URI: &str = "memory://knowledge-graph";

/// Environment variable that overrides the memory file location.
pub const MEMORY_FILE_PATH_ENV: &str = "MEMORY_FILE_PATH";

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for creating entities")]
pub struct CreateEntitiesArgs {
    #[schemars(description = "Entities to create")]
    pub entities: Vec<Entity>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for creating relations")]
pub struct CreateRelationsArgs {
    #[schemars(description = "Relations to create")]
    pub relations: Vec<Relation>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for adding observations")]
pub struct AddObservationsArgs {
    #[schemars(description = "Observations to add to existing entities")]
    pub observations: Vec<ObservationInput>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for deleting entities")]
pub struct DeleteEntitiesArgs {
    /// An array of entity names to delete
    #[serde(rename = "entityNames")]
    #[schemars(description = "An array of entity names to delete")]
    pub entity_names: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(description = "An observation deletion")]
pub struct ObservationDeletion {
    /// The name of the entity containing the observations
    #[schemars(description = "The name of the entity containing the observations")]
    pub entity_name: String,
    /// An array of observations to delete
    #[schemars(description = "An array of observations to delete")]
    pub observations: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for deleting observations")]
pub struct DeleteObservationsArgs {
    #[schemars(description = "Observations to delete from entities")]
    pub deletions: Vec<ObservationDeletion>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for deleting relations")]
pub struct DeleteRelationsArgs {
    #[schemars(description = "Relations to delete")]
    pub relations: Vec<Relation>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for searching the knowledge graph")]
pub struct SearchNodesArgs {
    /// The search query to match against entity names, types, and observation content
    #[schemars(
        description = "The search query to match against entity names, types, and observation content"
    )]
    pub query: String,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for opening nodes")]
pub struct OpenNodesArgs {
    /// An array of entity names to retrieve
    #[schemars(description = "An array of entity names to retrieve")]
    pub names: Vec<String>,
}

// Server

pub struct MemoryServer {
    manager: Arc<KnowledgeGraphManager>,
    notify_tx: broadcast::Sender<()>,
    tool_router: ToolRouter<MemoryServer>,
}

impl MemoryServer {
    pub fn new(manager: KnowledgeGraphManager) -> Self {
        let (notify_tx, _) = broadcast::channel(16);
        Self {
            manager: Arc::new(manager),
            notify_tx,
            tool_router: Self::tool_router(),
        }
    }

    /// Notify subscribed clients (modern `subscriptions/listen` and legacy
    /// `resources/subscribe`) that the graph changed.
    async fn notify_graph_updated(&self) {
        let _ = self.notify_tx.send(());
    }

    fn structured(value: serde_json::Value) -> CallToolResult {
        CallToolResult::structured(value)
    }
}

#[tool_router(router = tool_router)]
impl MemoryServer {
    #[tool(
        name = "create_entities",
        title = "Create Entities",
        description = "Create multiple new entities in the knowledge graph",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn create_entities(
        &self,
        Parameters(args): Parameters<CreateEntitiesArgs>,
    ) -> Result<CallToolResult, McpError> {
        let result = match self.manager.create_entities(args.entities).await {
            Ok(result) => result,
            Err(e) => return Ok(tool_error(e)),
        };
        self.notify_graph_updated().await;
        Ok(Self::structured(serde_json::json!({ "entities": result })))
    }

    #[tool(
        name = "create_relations",
        title = "Create Relations",
        description = "Create multiple new relations between entities in the knowledge graph. Relations should be in active voice",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn create_relations(
        &self,
        Parameters(args): Parameters<CreateRelationsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let result = match self.manager.create_relations(args.relations).await {
            Ok(result) => result,
            Err(e) => return Ok(tool_error(e)),
        };
        self.notify_graph_updated().await;
        Ok(Self::structured(serde_json::json!({ "relations": result })))
    }

    #[tool(
        name = "add_observations",
        title = "Add Observations",
        description = "Add new observations to existing entities in the knowledge graph",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn add_observations(
        &self,
        Parameters(args): Parameters<AddObservationsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let result: Vec<AddedObservation> =
            match self.manager.add_observations(args.observations).await {
                Ok(result) => result,
                Err(e) => return Ok(tool_error(e)),
            };
        self.notify_graph_updated().await;
        Ok(Self::structured(serde_json::json!({ "results": result })))
    }

    #[tool(
        name = "delete_entities",
        title = "Delete Entities",
        description = "Delete multiple entities and their associated relations from the knowledge graph",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn delete_entities(
        &self,
        Parameters(args): Parameters<DeleteEntitiesArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(e) = self.manager.delete_entities(args.entity_names).await {
            return Ok(tool_error(e));
        }
        self.notify_graph_updated().await;
        Ok(Self::structured(serde_json::json!({
            "success": true,
            "message": "Entities deleted successfully"
        })))
    }

    #[tool(
        name = "delete_observations",
        title = "Delete Observations",
        description = "Delete specific observations from entities in the knowledge graph",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn delete_observations(
        &self,
        Parameters(args): Parameters<DeleteObservationsArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(e) = self.manager.delete_observations(args.deletions).await {
            return Ok(tool_error(e));
        }
        self.notify_graph_updated().await;
        Ok(Self::structured(serde_json::json!({
            "success": true,
            "message": "Observations deleted successfully"
        })))
    }

    #[tool(
        name = "delete_relations",
        title = "Delete Relations",
        description = "Delete multiple relations from the knowledge graph",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn delete_relations(
        &self,
        Parameters(args): Parameters<DeleteRelationsArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(e) = self.manager.delete_relations(args.relations).await {
            return Ok(tool_error(e));
        }
        self.notify_graph_updated().await;
        Ok(Self::structured(serde_json::json!({
            "success": true,
            "message": "Relations deleted successfully"
        })))
    }

    #[tool(
        name = "read_graph",
        title = "Read Graph",
        description = "Read the entire knowledge graph",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn read_graph(&self) -> Result<CallToolResult, McpError> {
        let graph = match self.manager.read_graph().await {
            Ok(graph) => graph,
            Err(e) => return Ok(tool_error(e)),
        };
        Ok(Self::structured(serde_json::to_value(&graph).map_err(
            |e| McpError::internal_error(format!("Failed to serialize graph: {e}"), None),
        )?))
    }

    #[tool(
        name = "search_nodes",
        title = "Search Nodes",
        description = "Search for nodes in the knowledge graph based on a query",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn search_nodes(
        &self,
        Parameters(args): Parameters<SearchNodesArgs>,
    ) -> Result<CallToolResult, McpError> {
        let graph = match self.manager.search_nodes(&args.query).await {
            Ok(graph) => graph,
            Err(e) => return Ok(tool_error(e)),
        };
        Ok(Self::structured(serde_json::to_value(&graph).map_err(
            |e| McpError::internal_error(format!("Failed to serialize graph: {e}"), None),
        )?))
    }

    #[tool(
        name = "open_nodes",
        title = "Open Nodes",
        description = "Open specific nodes in the knowledge graph by their names",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn open_nodes(
        &self,
        Parameters(args): Parameters<OpenNodesArgs>,
    ) -> Result<CallToolResult, McpError> {
        let graph = match self.manager.open_nodes(args.names).await {
            Ok(graph) => graph,
            Err(e) => return Ok(tool_error(e)),
        };
        Ok(Self::structured(serde_json::to_value(&graph).map_err(
            |e| McpError::internal_error(format!("Failed to serialize graph: {e}"), None),
        )?))
    }
}

fn resource_text(graph: &KnowledgeGraph) -> String {
    serde_json::to_string_pretty(graph).expect("knowledge graph serializes")
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MemoryServer {
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
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_subscribe()
                .build(),
        )
        .with_server_info(Implementation::new("mcp-memory", env!("CARGO_PKG_VERSION")))
        .with_instructions(
            "This server provides persistent memory as a knowledge graph of entities, \
             relations, and observations. Use create_entities, create_relations and \
             add_observations to record information, read_graph to retrieve it, and \
             search_nodes or open_nodes to find specific entries.",
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: vec![
                Resource::new(RESOURCE_URI, "knowledge-graph")
                    .with_description("The full knowledge graph with all entities and relations")
                    .with_mime_type("application/json"),
            ],
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if request.uri != RESOURCE_URI {
            return Err(McpError::invalid_params(
                format!("Unknown resource URI: {}", request.uri),
                None,
            ));
        }
        let graph = self
            .manager
            .read_graph()
            .await
            .map_err(|e| McpError::internal_error(e, None))?;
        Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
            vec![ResourceContents::TextResourceContents {
                uri: RESOURCE_URI.to_string(),
                mime_type: Some("application/json".to_string()),
                text: resource_text(&graph),
                meta: None,
            }],
        )))
    }

    /// Modern (2026-07-28) subscription flow: accept updates for the
    /// knowledge-graph resource URI.
    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        let Some(uris) = &requested.resource_subscriptions else {
            return None;
        };
        let accepted: Vec<String> = uris
            .iter()
            .filter(|uri| uri.as_str() == RESOURCE_URI)
            .cloned()
            .collect();
        if accepted.is_empty() {
            None
        } else {
            Some(
                SubscriptionFilter::builder()
                    .resource_subscriptions(accepted)
                    .build(),
            )
        }
    }

    /// Hold the subscription open and forward graph-change notifications to
    /// the client whenever a mutation tool modifies the graph.
    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        let sink = context.sink().clone();
        let mut rx = self.notify_tx.subscribe();
        loop {
            tokio::select! {
                _ = context.cancelled() => return Ok(()),
                _ = rx.recv() => {
                    if let Err(error) = sink
                        .send(ServerNotification::ResourceUpdatedNotification(
                            ResourceUpdatedNotification::new(
                                ResourceUpdatedNotificationParam::new(RESOURCE_URI),
                            ),
                        ))
                        .await
                    {
                        return Err(McpError::internal_error(error.to_string(), None));
                    }
                }
            }
        }
    }
}

/// Resolve the memory file path: `--memory-file` option wins, then the
/// `MEMORY_FILE_PATH` environment variable, otherwise `memory.jsonl` in the
/// current working directory.
pub fn resolve_memory_file_path(cli_path: Option<PathBuf>) -> Result<PathBuf, String> {
    let path = match cli_path {
        Some(path) => path,
        None => match std::env::var_os(MEMORY_FILE_PATH_ENV) {
            Some(path) => PathBuf::from(path),
            None => PathBuf::from("memory.jsonl"),
        },
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|e| format!("Failed to resolve memory file path: {e}"))
    }
}

/// Start the memory server on stdio.
pub async fn run(memory_file: Option<PathBuf>) -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    use rmcp::transport::stdio;

    let path = resolve_memory_file_path(memory_file).map_err(|e| anyhow::anyhow!(e))?;
    let manager = KnowledgeGraphManager::new(path.clone());
    let server = MemoryServer::new(manager);

    let service = server
        .serve(stdio())
        .await
        .inspect_err(|e| tracing::error!("serving error: {e:?}"))?;
    tracing::info!(
        "Memory MCP server running on stdio (MCP {SPEC_VERSION}), memory file: {}",
        path.display()
    );

    service.waiting().await?;
    Ok(())
}
