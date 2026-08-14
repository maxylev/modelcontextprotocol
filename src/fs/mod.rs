mod edit;
mod format;
mod search;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, ContentBlock, Implementation, ResourceContents, ServerCapabilities,
        ServerInfo, Tool,
    },
    schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use crate::support::{AccessControl, SPEC_VERSION, text_result, tool_error};

use self::edit::{EditOperation, apply_edits, render_diff};
use self::format::{format_size, head_lines, tail_lines};
use self::search::{TreeEntry, directory_tree, search_files};

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for reading a text file")]
pub struct ReadTextFileArgs {
    /// File path to read, must be within an allowed directory
    pub path: String,
    /// If provided, returns only the first N lines of the file
    #[serde(default)]
    pub head: Option<u32>,
    /// If provided, returns only the last N lines of the file
    #[serde(default)]
    pub tail: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for reading a media file")]
pub struct ReadMediaFileArgs {
    /// File path to read, must be within an allowed directory
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for reading multiple files")]
pub struct ReadMultipleFilesArgs {
    /// Array of file paths to read. Each path must point to a valid file within allowed directories.
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for writing a file")]
pub struct WriteFileArgs {
    /// File location
    pub path: String,
    /// File content
    pub content: String,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for editing a file")]
pub struct EditFileArgs {
    /// File to edit
    pub path: String,
    /// List of edit operations
    pub edits: Vec<EditOperation>,
    /// Preview changes using git-style diff format without applying them
    #[serde(default, rename = "dryRun")]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for creating a directory")]
pub struct CreateDirectoryArgs {
    /// Directory path to create, must be within an allowed directory
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for listing a directory")]
pub struct ListDirectoryArgs {
    /// Directory path to list, must be within an allowed directory
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for listing a directory with sizes")]
pub struct ListDirectoryWithSizesArgs {
    /// Directory path to list, must be within an allowed directory
    pub path: String,
    /// Sort entries by name or size
    #[serde(default = "default_sort_by", rename = "sortBy")]
    pub sort_by: String,
}

fn default_sort_by() -> String {
    "name".to_string()
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for building a directory tree")]
pub struct DirectoryTreeArgs {
    /// Starting directory
    pub path: String,
    /// Exclude any paths matching these patterns
    #[serde(default, rename = "excludePatterns")]
    pub exclude_patterns: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for moving a file or directory")]
pub struct MoveFileArgs {
    /// Source file or directory
    pub source: String,
    /// Destination file or directory
    pub destination: String,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for searching files")]
pub struct SearchFilesArgs {
    /// Starting directory for the search
    pub path: String,
    /// Glob-style pattern to match, e.g. `*.rs` or `**/*.rs`
    pub pattern: String,
    /// Exclude any paths matching these patterns
    #[serde(default, rename = "excludePatterns")]
    pub exclude_patterns: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(description = "Arguments for getting file information")]
pub struct GetFileInfoArgs {
    /// File or directory path, must be within an allowed directory
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct FilesystemServer {
    access: Arc<AccessControl>,
    tool_router: ToolRouter<FilesystemServer>,
}

impl FilesystemServer {
    pub fn new(access: AccessControl) -> Self {
        Self {
            access: Arc::new(access),
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router(router = tool_router)]
impl FilesystemServer {
    #[tool(
        name = "read_file",
        title = "Read File (Deprecated)",
        description = "Read the complete contents of a file as text. DEPRECATED: Use read_text_file instead.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn read_file(
        &self,
        Parameters(args): Parameters<ReadTextFileArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.read_text_file_impl(args).await
    }

    #[tool(
        name = "read_text_file",
        title = "Read Text File",
        description = "Read the complete contents of a file from the file system as text. Handles various text encodings and provides detailed error messages if the file cannot be read. Use this tool when you need to examine the contents of a single file. Use the 'head' parameter to read only the first N lines of a file, or the 'tail' parameter to read only the last N lines of a file. Operates on the file as text regardless of extension. Only works within allowed directories.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn read_text_file(
        &self,
        Parameters(args): Parameters<ReadTextFileArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.read_text_file_impl(args).await
    }

    #[tool(
        name = "read_media_file",
        title = "Read Media File",
        description = "Read a file and return it as a base64-encoded content block with its MIME type. Image and audio files are returned as image/audio content; any other file type is returned as an embedded resource. Only works within allowed directories.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn read_media_file(
        &self,
        Parameters(args): Parameters<ReadMediaFileArgs>,
    ) -> Result<CallToolResult, McpError> {
        let valid_path = match self.resolve(&args.path).await {
            Ok(p) => p,
            Err(_) => return Ok(tool_error(access_denied(&args.path))),
        };
        let bytes = match tokio::fs::read(&valid_path).await {
            Ok(bytes) => bytes,
            Err(e) => {
                return Ok(tool_error(format!(
                    "Failed to read {}: {e}",
                    valid_path.display()
                )));
            }
        };
        let mime = mime_guess::from_path(&valid_path)
            .first_raw()
            .unwrap_or("application/octet-stream")
            .to_string();
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);

        let block = if mime.starts_with("image/") {
            ContentBlock::image(encoded, mime)
        } else if mime.starts_with("audio/") {
            ContentBlock::audio(encoded, mime)
        } else {
            let uri = url::Url::from_file_path(&valid_path)
                .map(|u| u.to_string())
                .unwrap_or_else(|_| format!("file://{}", valid_path.display()));
            ContentBlock::resource(ResourceContents::BlobResourceContents {
                uri,
                mime_type: Some(mime),
                blob: encoded,
                meta: None,
            })
        };
        Ok(CallToolResult::success(vec![block]))
    }

    #[tool(
        name = "read_multiple_files",
        title = "Read Multiple Files",
        description = "Read the contents of multiple files simultaneously. This is more efficient than reading files one by one when you need to analyze or compare multiple files. Each file's content is returned with its path as a reference. Failed reads for individual files won't stop the entire operation. Only works within allowed directories.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn read_multiple_files(
        &self,
        Parameters(args): Parameters<ReadMultipleFilesArgs>,
    ) -> Result<CallToolResult, McpError> {
        if args.paths.is_empty() {
            return Ok(tool_error("At least one file path must be provided"));
        }
        let mut results = Vec::with_capacity(args.paths.len());
        for file_path in &args.paths {
            match self.resolve(file_path).await {
                Ok(valid) => match tokio::fs::read_to_string(&valid).await {
                    Ok(content) => results.push(format!("{file_path}:\n{content}\n")),
                    Err(e) => results.push(format!("{file_path}: Error - {e}")),
                },
                Err(e) => results.push(format!("{file_path}: Error - {e}")),
            }
        }
        Ok(text_result(results.join("\n---\n")))
    }

    #[tool(
        name = "write_file",
        title = "Write File",
        description = "Create a new file or completely overwrite an existing file with new content. Use with caution as it will overwrite existing files without warning. Handles text content with proper encoding. Only works within allowed directories.",
        annotations(
            read_only_hint = false,
            idempotent_hint = true,
            destructive_hint = true,
            open_world_hint = false
        )
    )]
    async fn write_file(
        &self,
        Parameters(args): Parameters<WriteFileArgs>,
    ) -> Result<CallToolResult, McpError> {
        let valid_path = match self.resolve(&args.path).await {
            Ok(p) => p,
            Err(e) => return Ok(tool_error(e)),
        };
        // Atomic write via temp file + rename: replaces the target without
        // following a symlink that appears between validation and write.
        match atomic_write(&valid_path, args.content.as_bytes()).await {
            Ok(()) => Ok(text_result(format!("Successfully wrote to {}", args.path))),
            Err(e) => Ok(tool_error(format!(
                "Failed to write to {}: {e}",
                valid_path.display()
            ))),
        }
    }

    #[tool(
        name = "edit_file",
        title = "Edit File",
        description = "Make line-based edits to a text file. Each edit replaces exact line sequences with new content. Returns a git-style diff showing the changes made. Only works within allowed directories.",
        annotations(
            read_only_hint = false,
            idempotent_hint = false,
            destructive_hint = true,
            open_world_hint = false
        )
    )]
    async fn edit_file(
        &self,
        Parameters(args): Parameters<EditFileArgs>,
    ) -> Result<CallToolResult, McpError> {
        let valid_path = match self.resolve(&args.path).await {
            Ok(p) => p,
            Err(e) => return Ok(tool_error(e)),
        };
        let original = match tokio::fs::read_to_string(&valid_path).await {
            Ok(content) => content,
            Err(e) => {
                return Ok(tool_error(format!(
                    "Failed to read {}: {e}",
                    valid_path.display()
                )));
            }
        };
        let modified = match apply_edits(&original, &args.edits) {
            Ok(modified) => modified,
            Err(e) => return Ok(tool_error(e)),
        };
        let diff = render_diff(&original, &modified);

        if !args.dry_run
            && modified != original
            && let Err(e) = atomic_write(&valid_path, modified.as_bytes()).await
        {
            return Ok(tool_error(format!(
                "Failed to write {}: {e}",
                valid_path.display()
            )));
        }
        Ok(text_result(diff))
    }

    #[tool(
        name = "create_directory",
        title = "Create Directory",
        description = "Create a new directory or ensure a directory exists. Can create multiple nested directories in one operation. If the directory already exists, this operation will succeed silently. Perfect for setting up directory structures for projects or ensuring required paths exist. Only works within allowed directories.",
        annotations(
            read_only_hint = false,
            idempotent_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn create_directory(
        &self,
        Parameters(args): Parameters<CreateDirectoryArgs>,
    ) -> Result<CallToolResult, McpError> {
        let valid_path = match self.resolve(&args.path).await {
            Ok(p) => p,
            Err(e) => return Ok(tool_error(e)),
        };
        match tokio::fs::create_dir_all(&valid_path).await {
            Ok(()) => Ok(text_result(format!(
                "Successfully created directory {}",
                args.path
            ))),
            Err(e) => Ok(tool_error(format!(
                "Failed to create directory {}: {e}",
                valid_path.display()
            ))),
        }
    }

    #[tool(
        name = "list_directory",
        title = "List Directory",
        description = "Get a detailed listing of all files and directories in a specified path. Results clearly distinguish between files and directories with [FILE] and [DIR] prefixes. This tool is essential for understanding directory structure and finding specific files within a directory. Only works within allowed directories.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_directory(
        &self,
        Parameters(args): Parameters<ListDirectoryArgs>,
    ) -> Result<CallToolResult, McpError> {
        let valid_path = match self.resolve(&args.path).await {
            Ok(p) => p,
            Err(e) => return Ok(tool_error(e)),
        };
        let mut entries = match tokio::fs::read_dir(&valid_path).await {
            Ok(entries) => entries,
            Err(e) => {
                return Ok(tool_error(format!(
                    "Failed to list directory {}: {e}",
                    valid_path.display()
                )));
            }
        };
        let mut names = Vec::new();
        loop {
            let next = match entries.next_entry().await {
                Ok(next) => next,
                Err(e) => {
                    return Ok(tool_error(format!(
                        "Failed to list directory {}: {e}",
                        valid_path.display()
                    )));
                }
            };
            let Some(entry) = next else { break };
            let is_dir = match entry.file_type().await {
                Ok(t) => t.is_dir(),
                Err(e) => {
                    return Ok(tool_error(format!(
                        "Failed to list directory {}: {e}",
                        valid_path.display()
                    )));
                }
            };
            names.push(format!(
                "{} {}",
                if is_dir { "[DIR]" } else { "[FILE]" },
                entry.file_name().to_string_lossy()
            ));
        }
        names.sort();
        Ok(text_result(names.join("\n")))
    }

    #[tool(
        name = "list_directory_with_sizes",
        title = "List Directory with Sizes",
        description = "Get a detailed listing of all files and directories in a specified path, including sizes. Results clearly distinguish between files and directories with [FILE] and [DIR] prefixes. This tool is useful for understanding directory structure and finding specific files within a directory. Only works within allowed directories.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_directory_with_sizes(
        &self,
        Parameters(args): Parameters<ListDirectoryWithSizesArgs>,
    ) -> Result<CallToolResult, McpError> {
        if args.sort_by != "name" && args.sort_by != "size" {
            return Ok(tool_error(format!(
                "Invalid sortBy value {:?}: expected 'name' or 'size'",
                args.sort_by
            )));
        }
        let valid_path = match self.resolve(&args.path).await {
            Ok(p) => p,
            Err(e) => return Ok(tool_error(e)),
        };
        let mut entries = match tokio::fs::read_dir(&valid_path).await {
            Ok(entries) => entries,
            Err(e) => {
                return Ok(tool_error(format!(
                    "Failed to list directory {}: {e}",
                    valid_path.display()
                )));
            }
        };

        struct Entry {
            name: String,
            is_directory: bool,
            size: u64,
        }

        let mut detailed = Vec::new();
        loop {
            let next = match entries.next_entry().await {
                Ok(next) => next,
                Err(e) => {
                    return Ok(tool_error(format!(
                        "Failed to list directory {}: {e}",
                        valid_path.display()
                    )));
                }
            };
            let Some(entry) = next else { break };
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_directory = match entry.file_type().await {
                Ok(t) => t.is_dir(),
                Err(e) => {
                    return Ok(tool_error(format!(
                        "Failed to list directory {}: {e}",
                        valid_path.display()
                    )));
                }
            };
            let size = if is_directory {
                0
            } else {
                tokio::fs::metadata(entry.path())
                    .await
                    .map(|m| m.len())
                    .unwrap_or(0)
            };
            detailed.push(Entry {
                name,
                is_directory,
                size,
            });
        }

        detailed.sort_by(|a, b| {
            if args.sort_by == "size" {
                b.size.cmp(&a.size)
            } else {
                a.name.cmp(&b.name)
            }
        });

        let mut formatted = Vec::new();
        for entry in &detailed {
            let size = if entry.is_directory {
                String::new()
            } else {
                format_size(entry.size)
            };
            formatted.push(format!(
                "{} {:<30} {:>10}",
                if entry.is_directory {
                    "[DIR]"
                } else {
                    "[FILE]"
                },
                entry.name,
                size
            ));
        }

        let total_files = detailed.iter().filter(|e| !e.is_directory).count();
        let total_dirs = detailed.iter().filter(|e| e.is_directory).count();
        let total_size = detailed
            .iter()
            .filter(|e| !e.is_directory)
            .map(|e| e.size)
            .sum::<u64>();
        formatted.push(String::new());
        formatted.push(format!(
            "Total: {total_files} files, {total_dirs} directories"
        ));
        formatted.push(format!("Combined size: {}", format_size(total_size)));

        Ok(text_result(formatted.join("\n")))
    }

    #[tool(
        name = "directory_tree",
        title = "Directory Tree",
        description = "Get a recursive tree view of files and directories as a JSON structure. Each entry includes 'name', 'type' (file/directory), and 'children' for directories. Files have no children array, while directories always have a children array (which may be empty). The output is formatted with 2-space indentation for readability. Only works within allowed directories.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn directory_tree(
        &self,
        Parameters(args): Parameters<DirectoryTreeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let valid_path = match self.resolve(&args.path).await {
            Ok(p) => p,
            Err(e) => return Ok(tool_error(e)),
        };
        let access = self.access.clone();
        let tree: Vec<TreeEntry> =
            match directory_tree(&valid_path, &args.exclude_patterns, &access).await {
                Ok(tree) => tree,
                Err(e) => return Ok(tool_error(e)),
            };
        match serde_json::to_string_pretty(&tree) {
            Ok(json) => Ok(text_result(json)),
            Err(e) => Ok(tool_error(format!("Failed to serialize tree: {e}"))),
        }
    }

    #[tool(
        name = "move_file",
        title = "Move File",
        description = "Move or rename files and directories. Can move files between directories and rename them in a single operation. If the destination exists, the operation will fail. Works across different directories and can be used for simple renaming within the same directory. Both source and destination must be within allowed directories.",
        annotations(
            read_only_hint = false,
            idempotent_hint = false,
            destructive_hint = true,
            open_world_hint = false
        )
    )]
    async fn move_file(
        &self,
        Parameters(args): Parameters<MoveFileArgs>,
    ) -> Result<CallToolResult, McpError> {
        let valid_source = match self.resolve(&args.source).await {
            Ok(p) => p,
            Err(e) => return Ok(tool_error(e)),
        };
        let valid_dest = match self.resolve(&args.destination).await {
            Ok(p) => p,
            Err(e) => return Ok(tool_error(e)),
        };
        match tokio::fs::rename(&valid_source, &valid_dest).await {
            Ok(()) => Ok(text_result(format!(
                "Successfully moved {} to {}",
                args.source, args.destination
            ))),
            Err(e) => Ok(tool_error(format!(
                "Failed to move {} to {}: {e}",
                args.source, args.destination
            ))),
        }
    }

    #[tool(
        name = "search_files",
        title = "Search Files",
        description = "Recursively search for files and directories matching a pattern. The patterns should be glob-style patterns that match paths relative to the working directory. Use pattern like '*.ext' to match files in current directory, and '**/*.ext' to match files in all subdirectories. Returns full paths to all matching items. Great for finding files when you don't know their exact location. Only searches within allowed directories.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn search_files(
        &self,
        Parameters(args): Parameters<SearchFilesArgs>,
    ) -> Result<CallToolResult, McpError> {
        let valid_path = match self.resolve(&args.path).await {
            Ok(p) => p,
            Err(e) => return Ok(tool_error(e)),
        };
        let access = self.access.clone();
        let results =
            match search_files(&valid_path, &args.pattern, &args.exclude_patterns, &access).await {
                Ok(results) => results,
                Err(e) => return Ok(tool_error(e)),
            };
        if results.is_empty() {
            return Ok(text_result("No matches found"));
        }
        let text = results
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(text_result(text))
    }

    #[tool(
        name = "get_file_info",
        title = "Get File Info",
        description = "Retrieve detailed metadata about a file or directory. Returns comprehensive information including size, creation time, last modified time, permissions, and type. This tool is perfect for understanding file characteristics without reading the actual content. Only works within allowed directories.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_file_info(
        &self,
        Parameters(args): Parameters<GetFileInfoArgs>,
    ) -> Result<CallToolResult, McpError> {
        let valid_path = match self.resolve(&args.path).await {
            Ok(p) => p,
            Err(e) => return Ok(tool_error(e)),
        };
        let meta = match tokio::fs::metadata(&valid_path).await {
            Ok(meta) => meta,
            Err(e) => {
                return Ok(tool_error(format!(
                    "Failed to stat {}: {e}",
                    valid_path.display()
                )));
            }
        };
        let lines = [
            format!("size: {}", meta.len()),
            format!("created: {}", rfc3339(meta.created())),
            format!("modified: {}", rfc3339(meta.modified())),
            format!("accessed: {}", rfc3339(meta.accessed())),
            format!("isFile: {}", meta.is_file()),
            format!("isDirectory: {}", meta.is_dir()),
            format!("permissions: {}", permissions_string(&meta)),
        ];
        Ok(text_result(lines.join("\n")))
    }

    #[tool(
        name = "list_allowed_directories",
        title = "List Allowed Directories",
        description = "Returns the list of directories that this server is allowed to access. Subdirectories within these allowed directories are also accessible. Use this to understand which directories and their nested paths are available before trying to access files.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_allowed_directories(&self) -> Result<CallToolResult, McpError> {
        let text = format!(
            "Allowed directories:\n{}",
            self.access
                .roots()
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        );
        Ok(text_result(text))
    }
}

impl FilesystemServer {
    /// Resolve a user path against the allowed directories, or return an
    /// access-denied style error message.
    async fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        self.access.validate_path(path).await
    }

    async fn read_text_file_impl(
        &self,
        args: ReadTextFileArgs,
    ) -> Result<CallToolResult, McpError> {
        if args.head.is_some() && args.tail.is_some() {
            return Ok(tool_error(
                "Cannot specify both head and tail parameters simultaneously",
            ));
        }
        let valid_path = match self.resolve(&args.path).await {
            Ok(p) => p,
            Err(e) => return Ok(tool_error(e)),
        };
        let content = match tokio::fs::read_to_string(&valid_path).await {
            Ok(content) => content,
            Err(e) => {
                return Ok(tool_error(format!(
                    "Failed to read {}: {e}",
                    valid_path.display()
                )));
            }
        };
        let text = if let Some(n) = args.tail {
            tail_lines(&content, n)
        } else if let Some(n) = args.head {
            head_lines(&content, n)
        } else {
            content
        };
        Ok(text_result(text))
    }
}

#[cfg(unix)]
fn permissions_string(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    format!("{:o}", meta.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn permissions_string(_meta: &std::fs::Metadata) -> String {
    "unknown".to_string()
}

fn access_denied(path: &str) -> String {
    format!("Access denied - path outside allowed directories: {path}")
}

/// RFC 3339 (UTC) formatting for a `SystemTime`, without chrono.
fn rfc3339(time: std::io::Result<std::time::SystemTime>) -> String {
    time.map(|t| {
        let secs = t
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let days = (secs / 86_400) as i64;
        let rem = secs % 86_400;
        let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
        let (y, mo, d) = civil_from_days(days);
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
    })
    .unwrap_or_else(|_| "unknown".to_string())
}

/// Civil-from-days algorithm (Howard Hinnant), returns (year, month, day).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Replace `path` contents atomically: write a unique temp file next to the
/// target, then rename over it. Renames do not follow symlinks.
async fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut temp_name = path.as_os_str().to_os_string();
    temp_name.push(format!(".{unique}.tmp"));
    let temp = PathBuf::from(temp_name);

    let result = async {
        let mut file = tokio::fs::File::create(&temp).await?;
        file.write_all(content).await?;
        file.sync_all().await?;
        tokio::fs::rename(&temp, path).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for FilesystemServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "mcp-filesystem",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "This server provides secure file access restricted to the allowed \
                 directories passed on the command line. Use list_allowed_directories \
                 to see which directories are currently accessible.",
            )
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_router.get(name).cloned()
    }
}

/// Start the filesystem server on stdio.
pub async fn run(dirs: Vec<PathBuf>) -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    use rmcp::transport::stdio;

    let access = match AccessControl::from_args(&dirs) {
        Ok(access) => access,
        Err(message) => {
            eprintln!("Error: {message}");
            eprintln!(
                "Usage: modelcontextprotocol filesystem [allowed-directory] [additional-directories...]"
            );
            std::process::exit(1);
        }
    };

    let server = FilesystemServer::new(access);
    let service = server
        .serve(stdio())
        .await
        .inspect_err(|e| tracing::error!("serving error: {e:?}"))?;
    tracing::info!("Filesystem MCP server running on stdio (MCP {SPEC_VERSION})");

    service.waiting().await?;
    Ok(())
}
