use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Filesystem, Fetch, Memory, Shell, Skills, and Agents MCP servers in a single binary.
///
/// Implements the Model Context Protocol (2026-07-28) over stdio. Both the
/// subcommand form (`modelcontextprotocol filesystem <dir>`) and the
/// flag form (`modelcontextprotocol --filesystem <dir>`) are supported so the
/// binary can be wired into any MCP client configuration.
#[derive(Debug, Parser)]
#[command(name = "modelcontextprotocol", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Start the filesystem server with these allowed directories
    /// (equivalent to `filesystem <DIR>...`).
    #[arg(long, value_name = "DIR", num_args = 1..)]
    pub filesystem: Option<Vec<PathBuf>>,

    /// Start the fetch server (equivalent to `fetch`).
    #[arg(long)]
    pub fetch: bool,

    /// Start the memory server (equivalent to `memory`).
    #[arg(long)]
    pub memory: bool,

    /// Start the shell server with these allowed directories
    /// (equivalent to `shell <DIR>...`).
    #[arg(long, value_name = "DIR", num_args = 1..)]
    pub shell: Option<Vec<PathBuf>>,

    /// Start the skills server for this workspace (equivalent to `skills <DIR>`).
    #[arg(long, value_name = "DIR")]
    pub skills: Option<PathBuf>,

    /// Start the agents server for this workspace (equivalent to `agents <DIR>`).
    #[arg(long, value_name = "DIR")]
    pub agents: Option<PathBuf>,

    /// Memory file location (JSONL), used by the memory server.
    #[arg(long, value_name = "PATH")]
    pub memory_file: Option<PathBuf>,

    #[command(flatten)]
    pub fetch_options: FetchOptions,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start the filesystem MCP server.
    ///
    /// All file operations are restricted to the directories passed as
    /// arguments. At least one directory is required.
    Filesystem {
        /// Directories the server is allowed to read and write.
        #[arg(value_name = "DIR", required = true, num_args = 1..)]
        dirs: Vec<PathBuf>,
    },
    /// Start the fetch MCP server.
    Fetch(FetchOptions),
    /// Start the memory MCP server.
    ///
    /// Persistent knowledge-graph memory stored as JSONL. The file location
    /// defaults to `memory.jsonl` in the current directory, and can be
    /// overridden with `--memory-file` or the `MEMORY_FILE_PATH` environment
    /// variable.
    Memory {
        /// Location of the memory JSONL file.
        #[arg(long, value_name = "PATH")]
        memory_file: Option<PathBuf>,
    },
    /// Start the shell MCP server.
    ///
    /// Executes local programs directly (no shell) with the OS permissions of
    /// the MCP server process. The working directory of every command is
    /// restricted to the directories passed as arguments. At least one
    /// directory is required.
    Shell {
        /// Directories the server allows commands to run in.
        #[arg(value_name = "DIR", required = true, num_args = 1..)]
        dirs: Vec<PathBuf>,
    },
    /// Start the Agent Skills MCP server for one workspace.
    Skills {
        /// Workspace root containing project-local skill definitions.
        #[arg(value_name = "DIR")]
        dir: PathBuf,
    },
    /// Start the local subagents MCP server for one workspace.
    Agents {
        /// Workspace root containing project-local agent definitions.
        #[arg(value_name = "DIR")]
        dir: PathBuf,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Args)]
pub struct FetchOptions {
    /// Ignore robots.txt restrictions when fetching on behalf of the model.
    #[arg(long)]
    pub ignore_robots_txt: bool,

    /// Custom User-Agent header used for all requests.
    #[arg(long, value_name = "USER_AGENT")]
    pub user_agent: Option<String>,

    /// Route all requests through this HTTP(S) proxy.
    #[arg(long, value_name = "URL")]
    pub proxy_url: Option<String>,
}

impl Cli {
    /// Normalize the two supported invocation styles into a single command.
    ///
    /// Exactly one server selector (subcommand or top-level flag) must be
    /// present, and server-specific options may only be combined with the
    /// server they belong to. Anything else — e.g. `--ignore-robots-txt`
    /// with `filesystem`, or `--memory-file` with `fetch` — returns `None`
    /// so the caller can fail loudly instead of silently ignoring options.
    pub fn into_command(self) -> Option<Command> {
        let has_fetch_options = self.fetch_options != FetchOptions::default();
        match (
            self.command,
            self.filesystem,
            self.fetch,
            self.memory,
            self.shell,
            self.skills,
            self.agents,
        ) {
            (Some(command), None, false, false, None, None, None) => {
                if has_fetch_options || self.memory_file.is_some() {
                    None
                } else {
                    Some(command)
                }
            }
            (None, Some(dirs), false, false, None, None, None) => {
                if has_fetch_options || self.memory_file.is_some() {
                    None
                } else {
                    Some(Command::Filesystem { dirs })
                }
            }
            (None, None, true, false, None, None, None) => {
                if self.memory_file.is_some() {
                    None
                } else {
                    Some(Command::Fetch(self.fetch_options))
                }
            }
            (None, None, false, true, None, None, None) => {
                if has_fetch_options {
                    None
                } else {
                    Some(Command::Memory {
                        memory_file: self.memory_file,
                    })
                }
            }
            (None, None, false, false, Some(dirs), None, None) => {
                if has_fetch_options || self.memory_file.is_some() {
                    None
                } else {
                    Some(Command::Shell { dirs })
                }
            }
            (None, None, false, false, None, Some(dir), None) => {
                if has_fetch_options || self.memory_file.is_some() {
                    None
                } else {
                    Some(Command::Skills { dir })
                }
            }
            (None, None, false, false, None, None, Some(dir)) => {
                if has_fetch_options || self.memory_file.is_some() {
                    None
                } else {
                    Some(Command::Agents { dir })
                }
            }
            _ => None,
        }
    }
}

pub fn print_usage() {
    eprintln!(
        "modelcontextprotocol: exactly one server must be selected, and server-specific \
         options must belong to the selected server.\n\n\
         Usage:\n  \
         modelcontextprotocol filesystem <DIR> [DIR ...]\n  \
         modelcontextprotocol fetch [--ignore-robots-txt] [--user-agent <UA>] [--proxy-url <URL>]\n  \
         modelcontextprotocol memory [--memory-file <PATH>]\n  \
         modelcontextprotocol shell <DIR> [DIR ...]\n\n\
         modelcontextprotocol skills <DIR>\n  \
         modelcontextprotocol agents <DIR>\n\n\
         Equivalent flag forms:\n  \
         modelcontextprotocol --filesystem <DIR> [DIR ...]\n  \
         modelcontextprotocol --fetch [--ignore-robots-txt] [--user-agent <UA>] [--proxy-url <URL>]\n  \
         modelcontextprotocol --memory [--memory-file <PATH>]\n  \
         modelcontextprotocol --shell <DIR> [DIR ...]\n  \
         modelcontextprotocol --skills <DIR>\n  \
         modelcontextprotocol --agents <DIR>"
    );
}
