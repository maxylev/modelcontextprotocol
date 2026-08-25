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

    /// Start the filesystem server with these allowed directories, defaulting
    /// to the current directory (equivalent to `filesystem [DIR]...`).
    #[arg(long, value_name = "DIR", num_args = 0.., default_missing_value = ".")]
    pub filesystem: Option<Vec<PathBuf>>,

    /// Start the fetch server (equivalent to `fetch`).
    #[arg(long)]
    pub fetch: bool,

    /// Start the memory server (equivalent to `memory`).
    #[arg(long)]
    pub memory: bool,

    /// Start the shell server with these allowed directories, defaulting to
    /// the current directory (equivalent to `shell [DIR]...`).
    #[arg(long, value_name = "DIR", num_args = 0.., default_missing_value = ".")]
    pub shell: Option<Vec<PathBuf>>,

    /// Start the skills server for a workspace, defaulting to the current directory.
    #[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = ".")]
    pub skills: Option<PathBuf>,

    /// Start the agents server for a workspace, defaulting to the current directory.
    #[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = ".")]
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
    /// arguments, or to the current directory when none are passed.
    Filesystem {
        /// Directories the server is allowed to read and write.
        #[arg(value_name = "DIR", num_args = 0.., default_value = ".")]
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
    /// restricted to the directories passed as arguments, or to the current
    /// directory when none are passed.
    Shell {
        /// Directories the server allows commands to run in.
        #[arg(value_name = "DIR", num_args = 0.., default_value = ".")]
        dirs: Vec<PathBuf>,
    },
    /// Start the Agent Skills MCP server for one workspace.
    Skills {
        /// Workspace root containing project-local skill definitions.
        #[arg(value_name = "DIR", default_value = ".")]
        dir: PathBuf,
    },
    /// Start the local subagents MCP server for one workspace.
    Agents {
        /// Workspace root containing project-local agent definitions.
        #[arg(value_name = "DIR", default_value = ".")]
        dir: PathBuf,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Args)]
pub struct FetchOptions {
    /// Respect robots.txt restrictions when fetching on behalf of the model.
    #[arg(long)]
    pub respect_robots_txt: bool,

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
    /// server they belong to. Anything else — e.g. `--respect-robots-txt`
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
         modelcontextprotocol filesystem [DIR ...]\n  \
         modelcontextprotocol fetch [--respect-robots-txt] [--user-agent <UA>] [--proxy-url <URL>]\n  \
         modelcontextprotocol memory [--memory-file <PATH>]\n  \
         modelcontextprotocol shell [DIR ...]\n\n\
         modelcontextprotocol skills [DIR]\n  \
         modelcontextprotocol agents [DIR]\n\n\
         Equivalent flag forms:\n  \
         modelcontextprotocol --filesystem [DIR ...]\n  \
         modelcontextprotocol --fetch [--respect-robots-txt] [--user-agent <UA>] [--proxy-url <URL>]\n  \
         modelcontextprotocol --memory [--memory-file <PATH>]\n  \
         modelcontextprotocol --shell [DIR ...]\n  \
         modelcontextprotocol --skills [DIR]\n  \
         modelcontextprotocol --agents [DIR]"
    );
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command};
    use clap::Parser;
    use std::path::PathBuf;

    #[test]
    fn workspace_servers_default_to_current_directory() {
        for args in [
            &["modelcontextprotocol", "filesystem"][..],
            &["modelcontextprotocol", "--filesystem"][..],
            &["modelcontextprotocol", "shell"][..],
            &["modelcontextprotocol", "--shell"][..],
            &["modelcontextprotocol", "skills"][..],
            &["modelcontextprotocol", "--skills"][..],
            &["modelcontextprotocol", "agents"][..],
            &["modelcontextprotocol", "--agents"][..],
        ] {
            let command = Cli::try_parse_from(args)
                .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"))
                .into_command()
                .unwrap_or_else(|| panic!("failed to select a command for {args:?}"));

            match command {
                Command::Filesystem { dirs } | Command::Shell { dirs } => {
                    assert_eq!(dirs, [PathBuf::from(".")]);
                }
                Command::Skills { dir } | Command::Agents { dir } => {
                    assert_eq!(dir, PathBuf::from("."));
                }
                Command::Fetch(_) | Command::Memory { .. } => {
                    panic!("unexpected command for {args:?}");
                }
            }
        }
    }
}
