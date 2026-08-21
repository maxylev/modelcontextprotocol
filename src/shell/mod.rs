mod drain;

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::support::{AccessControl, SPEC_VERSION, tool_error};

use self::drain::drain_limited;

/// Default and maximum execution time in milliseconds.
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
pub const MAX_TIMEOUT_MS: u64 = 600_000;

/// Maximum number of bytes retained per captured stream. After this limit the
/// pipe keeps being drained, but additional bytes are discarded.
const STREAM_CAPTURE_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for executing a command")]
pub struct ExecuteCommandArgs {
    /// The executable to run, resolved through PATH like a direct exec. Never
    /// a shell command string.
    #[schemars(
        description = "The executable to run, resolved through PATH like a direct exec. Never a shell command string."
    )]
    pub program: String,
    /// Arguments passed to the executable, each preserved exactly as given
    /// (no shell parsing, quoting, or glob expansion)
    #[serde(default)]
    #[schemars(
        description = "Arguments passed to the executable, each preserved exactly as given (no shell parsing, quoting, or glob expansion)"
    )]
    pub args: Vec<String>,
    /// Working directory for the command. Must resolve inside one of the
    /// allowed directories. Relative paths resolve against the first allowed
    /// directory. Defaults to the first allowed directory.
    #[serde(default)]
    #[schemars(
        description = "Working directory for the command. Must resolve inside one of the allowed directories. Relative paths resolve against the first allowed directory. Defaults to the first allowed directory."
    )]
    pub cwd: Option<String>,
    /// Maximum execution time in milliseconds (1 to 600000, default 120000).
    /// On expiry the process is terminated and `timed_out` is reported.
    #[serde(default = "default_timeout_ms")]
    #[schemars(range(min = 1, max = 600000))]
    #[schemars(
        description = "Maximum execution time in milliseconds (1 to 600000, default 120000). On expiry the process is terminated and `timed_out` is reported."
    )]
    pub timeout_ms: u64,
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

/// Structured result of a completed (or timed out) command execution.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[schemars(description = "The result of a command execution")]
pub struct CommandOutput {
    /// The child's numeric exit code, or null when no normal exit code is
    /// available (e.g. the process was terminated on timeout)
    #[schemars(
        description = "The child's numeric exit code, or null when no normal exit code is available (e.g. the process was terminated on timeout)"
    )]
    pub exit_code: Option<i32>,
    /// Captured standard output, lossy UTF-8, bounded to 1 MiB
    #[schemars(description = "Captured standard output, lossy UTF-8, bounded to 1 MiB")]
    pub stdout: String,
    /// Captured standard error, lossy UTF-8, bounded to 1 MiB
    #[schemars(description = "Captured standard error, lossy UTF-8, bounded to 1 MiB")]
    pub stderr: String,
    /// True when the command was terminated because it exceeded `timeout_ms`
    #[schemars(
        description = "True when the command was terminated because it exceeded `timeout_ms`"
    )]
    pub timed_out: bool,
    /// True when stdout exceeded the 1 MiB capture limit and was truncated
    #[schemars(
        description = "True when stdout exceeded the 1 MiB capture limit and was truncated"
    )]
    pub stdout_truncated: bool,
    /// True when stderr exceeded the 1 MiB capture limit and was truncated
    #[schemars(
        description = "True when stderr exceeded the 1 MiB capture limit and was truncated"
    )]
    pub stderr_truncated: bool,
}

#[derive(Debug, Clone)]
pub struct ShellServer {
    access: Arc<AccessControl>,
    tool_router: ToolRouter<ShellServer>,
}

impl ShellServer {
    pub fn new(access: AccessControl) -> Self {
        Self {
            access: Arc::new(access),
            tool_router: Self::tool_router(),
        }
    }

    /// Resolve and verify `cwd` against the allowed directories. `None` falls
    /// back to the first allowed directory.
    async fn resolve_cwd(&self, cwd: Option<&str>) -> Result<PathBuf, String> {
        let requested = cwd.unwrap_or(".");
        let resolved = self.access.validate_path(requested).await?;
        let meta = tokio::fs::metadata(&resolved)
            .await
            .map_err(|e| format!("Failed to access cwd {}: {e}", resolved.display()))?;
        if !meta.is_dir() {
            return Err(format!("cwd is not a directory: {}", resolved.display()));
        }
        Ok(resolved)
    }

    /// Concise text representation for clients that primarily consume MCP
    /// text content; the full payload is in `structuredContent`.
    fn summary_text(output: &CommandOutput) -> String {
        let status = if output.timed_out {
            "command timed out - process terminated".to_string()
        } else {
            match output.exit_code {
                Some(code) => format!("exit code: {code}"),
                None => "exit code: (none)".to_string(),
            }
        };
        let stdout_note = if output.stdout_truncated {
            "truncated at 1 MiB"
        } else {
            "full"
        };
        let stderr_note = if output.stderr_truncated {
            "truncated at 1 MiB"
        } else {
            "full"
        };
        format!(
            "{status}\nstdout ({stdout_note}): {}\nstderr ({stderr_note}): {}",
            preview(&output.stdout),
            preview(&output.stderr)
        )
    }
}

fn preview(text: &str) -> String {
    const PREVIEW_MAX: usize = 4000;
    let count = text.chars().count();
    if count > PREVIEW_MAX {
        let truncated: String = text.chars().take(PREVIEW_MAX).collect();
        format!("{truncated}…")
    } else {
        text.to_string()
    }
}

#[tool_router(router = tool_router)]
impl ShellServer {
    #[tool(
        name = "execute_command",
        title = "Execute Command",
        description = "Execute one local program directly with an explicit argv and wait for it to finish or time out. The program is resolved through PATH and spawned without a shell: no shell syntax, quoting, or glob expansion is applied, and the argument list is passed exactly as given. If shell features are required, run an installed shell explicitly, e.g. program=\"bash\" with args=[\"-lc\", \"cargo test && git status\"]. The working directory must be inside one of the server's allowed directories. Stdout and stderr are captured separately, each bounded to 1 MiB. On timeout the process is terminated and timed_out is reported. This tool executes arbitrary local programs with the permissions of the MCP server process.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<CommandOutput>(),
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = false, open_world_hint = true)
    )]
    async fn execute_command(
        &self,
        Parameters(args): Parameters<ExecuteCommandArgs>,
    ) -> Result<CallToolResult, McpError> {
        let program = args.program.trim();
        if program.is_empty() {
            return Ok(tool_error(
                "program must be a non-empty executable name or path",
            ));
        }
        if !(1..=MAX_TIMEOUT_MS).contains(&args.timeout_ms) {
            return Ok(tool_error(format!(
                "timeout_ms must be between 1 and {MAX_TIMEOUT_MS}, got {}",
                args.timeout_ms
            )));
        }
        let cwd = match self.resolve_cwd(args.cwd.as_deref()).await {
            Ok(cwd) => cwd,
            Err(e) => return Ok(tool_error(e)),
        };

        let mut command = Command::new(program);
        command
            .args(&args.args)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // Reap the child even if this future is cancelled or dropped.
            .kill_on_drop(true);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                return Ok(tool_error(format!("Failed to spawn {program}: {e}")));
            }
        };

        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let stdout_task = tokio::spawn(drain_limited(stdout, STREAM_CAPTURE_LIMIT));
        let stderr_task = tokio::spawn(drain_limited(stderr, STREAM_CAPTURE_LIMIT));

        let timeout = std::time::Duration::from_millis(args.timeout_ms);
        let mut timed_out = false;
        let exit_code = match tokio::time::timeout(timeout, child.wait()).await {
            Ok(Ok(status)) => status.code(),
            Ok(Err(e)) => {
                // The wait itself failed; drain what was captured and report.
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Ok(tool_error(format!("Failed to wait for {program}: {e}")));
            }
            Err(_) => {
                // Timeout: terminate and reap the direct child.
                timed_out = true;
                let _ = child.kill().await;
                let _ = child.wait().await;
                None
            }
        };

        // The pipes close once the child is gone; the drain tasks finish and
        // the captured streams are bounded to the capture limit.
        let (stdout, stdout_truncated) = stdout_task.await.unwrap_or_default();
        let (stderr, stderr_truncated) = stderr_task.await.unwrap_or_default();

        let output = CommandOutput {
            exit_code,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            timed_out,
            stdout_truncated,
            stderr_truncated,
        };

        // Structured result with the declared output schema, plus a concise
        // text representation for text-only clients.
        let mut result =
            CallToolResult::structured(serde_json::to_value(&output).map_err(|e| {
                McpError::internal_error(format!("Failed to serialize result: {e}"), None)
            })?);
        result.content = vec![ContentBlock::text(Self::summary_text(&output))];
        Ok(result)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ShellServer {
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
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("mcp-shell", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "This server executes local programs directly (no shell) with the OS \
                 permissions of the MCP server process. The working directory is \
                 restricted to the allowed directories passed on the command line.",
            )
    }
}

/// Start the shell server on stdio.
pub async fn run(dirs: Vec<PathBuf>) -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    use rmcp::transport::stdio;

    let access = match AccessControl::from_args(&dirs) {
        Ok(access) => access,
        Err(message) => {
            eprintln!("Error: {message}");
            eprintln!(
                "Usage: modelcontextprotocol shell [allowed-directory] [additional-directories...]"
            );
            std::process::exit(1);
        }
    };

    let server = ShellServer::new(access);
    let service = server
        .serve(stdio())
        .await
        .inspect_err(|e| tracing::error!("serving error: {e:?}"))?;
    tracing::info!("Shell MCP server running on stdio (MCP {SPEC_VERSION})");

    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_text_reports_exit_code() {
        let output = CommandOutput {
            exit_code: Some(0),
            stdout: "out".to_string(),
            stderr: String::new(),
            timed_out: false,
            stdout_truncated: false,
            stderr_truncated: false,
        };
        let text = ShellServer::summary_text(&output);
        assert!(text.contains("exit code: 0"));
        assert!(text.contains("stdout (full): out"));
    }

    #[test]
    fn summary_text_reports_timeout_and_truncation() {
        let output = CommandOutput {
            exit_code: None,
            stdout: "x".repeat(5000),
            stderr: String::new(),
            timed_out: true,
            stdout_truncated: true,
            stderr_truncated: false,
        };
        let text = ShellServer::summary_text(&output);
        assert!(text.contains("timed out"));
        assert!(text.contains("stdout (truncated at 1 MiB)"));
        assert!(text.contains('…'), "long output is previewed");
        assert!(!text.contains("exit code"), "no exit code on timeout");
    }

    #[test]
    fn preview_keeps_short_text_intact() {
        assert_eq!(preview("short"), "short");
        let long = "a".repeat(5000);
        let p = preview(&long);
        assert_eq!(p.chars().count(), 4001, "4000 chars plus ellipsis");
        assert!(p.ends_with('…'));
    }
}
