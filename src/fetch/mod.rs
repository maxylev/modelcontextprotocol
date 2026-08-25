mod http;

use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::router::prompt::PromptRouter,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, ContentBlock, GetPromptResult, Implementation, PromptMessage, Role,
        ServerCapabilities, ServerInfo,
    },
    prompt, prompt_handler, prompt_router, schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;

use crate::cli::FetchOptions;
use crate::support::{SPEC_VERSION, tool_error};

use self::http::{DEFAULT_USER_AGENT, FetchClient, check_may_fetch, fetch_url, truncate};

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Parameters for fetching a URL")]
pub struct FetchArgs {
    /// URL to fetch
    pub url: String,
    /// Maximum number of characters to return
    #[serde(default = "default_max_length")]
    #[schemars(range(min = 1, max = 999999))]
    pub max_length: i64,
    /// Start content from this character index, useful if a previous fetch was
    /// truncated and more context is required
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub start_index: i64,
    /// Get the actual content of the requested page without simplification
    #[serde(default)]
    pub raw: bool,
}

fn default_max_length() -> i64 {
    5000
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for the fetch prompt")]
pub struct FetchPromptArgs {
    /// URL to fetch
    pub url: String,
}

pub struct FetchServer {
    client: Arc<FetchClient>,
    user_agent: String,
    respect_robots_txt: bool,
    tool_router: ToolRouter<FetchServer>,
    prompt_router: PromptRouter<FetchServer>,
}

impl FetchServer {
    pub fn new(client: FetchClient, user_agent: String, respect_robots_txt: bool) -> Self {
        Self {
            client: Arc::new(client),
            user_agent,
            respect_robots_txt,
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
        }
    }
}

#[tool_router(router = tool_router)]
impl FetchServer {
    #[tool(
        name = "fetch",
        title = "Fetch",
        description = "Fetches a URL from the internet and optionally extracts its contents as markdown. This tool grants you internet access for retrieving up-to-date information. To search the web, fetch https://lite.duckduckgo.com/lite/?q={url_encoded_query}&kl={region_language}&kp={safe_search}, for example https://lite.duckduckgo.com/lite/?q=mcp&kl=us-en&kp=-2. Use kl such as us-en for region/language and kp=1 for Safe Search on, kp=-1 for moderate, or kp=-2 for off.",
        annotations(open_world_hint = true)
    )]
    async fn fetch(
        &self,
        Parameters(args): Parameters<FetchArgs>,
    ) -> Result<CallToolResult, McpError> {
        let url = args.url;

        let parsed = match url::Url::parse(&url) {
            Ok(parsed) => parsed,
            Err(e) => {
                return Ok(tool_error(format!("Invalid URL {url:?}: {e}")));
            }
        };
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Ok(tool_error(format!(
                "Unsupported URL scheme {:?} - only http and https are supported",
                parsed.scheme()
            )));
        }
        if !(1..1_000_000).contains(&args.max_length) {
            return Ok(tool_error(format!(
                "max_length must be between 1 and 999999, got {}",
                args.max_length
            )));
        }
        if args.start_index < 0 {
            return Ok(tool_error(format!(
                "start_index must be non-negative, got {}",
                args.start_index
            )));
        }

        if self.respect_robots_txt
            && let Err(e) = check_may_fetch(&self.client, &url, &self.user_agent).await
        {
            return Ok(tool_error(e));
        }

        let (content, prefix) =
            match fetch_url(&self.client, &url, &self.user_agent, args.raw).await {
                Ok(result) => result,
                Err(e) => return Ok(tool_error(e)),
            };

        let content = truncate(
            &content,
            args.start_index as usize,
            args.max_length as usize,
        );
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "{prefix}Contents of {url}:\n{content}"
        ))]))
    }
}

#[prompt_router]
impl FetchServer {
    #[prompt(
        name = "fetch",
        description = "Fetch a URL and extract its contents as markdown"
    )]
    async fn fetch_prompt(
        &self,
        Parameters(args): Parameters<FetchPromptArgs>,
    ) -> Result<GetPromptResult, McpError> {
        if args.url.trim().is_empty() {
            return Err(McpError::invalid_params("URL is required", None));
        }
        let url = args.url;
        // User-initiated fetches always skip the robots.txt check.
        match fetch_url(&self.client, &url, &self.user_agent, false).await {
            Ok((content, prefix)) => Ok(GetPromptResult::new(vec![PromptMessage::new_text(
                Role::User,
                format!("{prefix}{content}"),
            )])
            .with_description(format!("Contents of {url}"))),
            Err(e) => Ok(
                GetPromptResult::new(vec![PromptMessage::new_text(Role::User, e)])
                    .with_description(format!("Failed to fetch {url}")),
            ),
        }
    }
}

#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for FetchServer {
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
                .enable_prompts()
                .build(),
        )
        .with_server_info(Implementation::new("mcp-fetch", env!("CARGO_PKG_VERSION")))
        .with_instructions(
            "This server fetches web content and converts HTML pages to markdown. \
                 The fetch tool ignores robots.txt by default; start the server with \
                 --respect-robots-txt to enforce it. The fetch prompt always fetches without \
                 checking robots.txt.",
        )
    }
}

/// Start the fetch server on stdio.
pub async fn run(options: FetchOptions) -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    use rmcp::transport::stdio;

    let user_agent = options
        .user_agent
        .unwrap_or_else(|| DEFAULT_USER_AGENT.to_string());

    let client = FetchClient::new(options.proxy_url.as_deref()).map_err(|e| anyhow::anyhow!(e))?;
    let server = FetchServer::new(client, user_agent, options.respect_robots_txt);

    let service = server
        .serve(stdio())
        .await
        .inspect_err(|e| tracing::error!("serving error: {e:?}"))?;
    tracing::info!("Fetch MCP server running on stdio (MCP {SPEC_VERSION})");

    service.waiting().await?;
    Ok(())
}
