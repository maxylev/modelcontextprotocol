//! Neutral support shared by the concrete MCP servers.
//!
//! Dependency direction: `main`/CLI -> concrete servers (`fs`, `fetch`,
//! `memory`, `shell`) -> this module -> external crates. Concrete servers
//! never depend on one another; everything they share lives here.

mod access;

pub use access::AccessControl;

use std::path::{Path, PathBuf};

use rmcp::model::{CallToolResult, ContentBlock, ProtocolVersion};
use tokio::io::AsyncWriteExt;

/// The MCP protocol version implemented by every server in this binary.
pub const SPEC_VERSION: &str = "2026-07-28";

/// The only protocol revision supported by this binary.
pub static SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[ProtocolVersion::V_2026_07_28];

/// Reject the legacy initialize lifecycle used by pre-2026 protocol clients.
pub fn reject_legacy_initialize()
-> std::future::Ready<Result<rmcp::model::InitializeResult, rmcp::ErrorData>> {
    std::future::ready(Err(rmcp::ErrorData::method_not_found::<
        rmcp::model::InitializeResultMethod,
    >()))
}

/// A tool-level error result carrying a plain-text message.
pub fn tool_error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// A successful tool result carrying plain text content.
pub fn text_result(text: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text)])
}

/// Replace `path` contents atomically: write a unique temp file in the same
/// directory as the target, flush it to disk, then rename it over the
/// target. The previous target contents are never replaced before the new
/// contents have been fully written, and are preserved when the temporary
/// write fails. The temporary file is removed on failure where possible.
///
/// When the target already exists as a regular file, its permissions are
/// captured and applied to the temp file before the rename so an existing
/// file keeps its mode (e.g. a 0600 memory file stays private). A brand-new
/// target uses the normal permissions produced by file creation under the
/// process umask.
///
/// Windows note: `tokio::fs::rename` replaces an existing destination file
/// on Windows (via `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`), so
/// file-for-file replacement works on every CI target. Renames do not follow
/// symlinks.
pub async fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut temp_name = path.as_os_str().to_os_string();
    temp_name.push(format!(".{unique}.{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp_name);

    // Captured before writing so an existing file's mode survives the rename.
    let existing_permissions = capture_existing_permissions(path);
    let result = async {
        let mut file = tokio::fs::File::create(&temp).await?;
        file.write_all(content).await?;
        if let Some(permissions) = existing_permissions {
            file.set_permissions(permissions).await?;
        }
        file.sync_all().await?;
        tokio::fs::rename(&temp, path).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}

/// Capture an existing regular file's permissions for reuse on the temp
/// file. Returns `None` when the target does not exist or is not a regular
/// file (e.g. a symlink); callers must not fail on that.
#[cfg(unix)]
fn capture_existing_permissions(path: &Path) -> Option<std::fs::Permissions> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    Some(metadata.permissions())
}

#[cfg(not(unix))]
fn capture_existing_permissions(_path: &Path) -> Option<std::fs::Permissions> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn atomic_rewrite_preserves_existing_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("target.txt");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        atomic_write(&path, b"new contents").await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new contents");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode {mode:#o}");
    }

    #[tokio::test]
    async fn new_file_uses_normal_creation_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fresh.txt");
        atomic_write(&path, b"x").await.unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        // A normal file create always grants the owner read/write under any
        // reasonable umask; a too-narrow mode would mean we pinned a stale
        // permission instead of creating fresh.
        assert_eq!(mode & 0o600, 0o600, "mode {mode:#o}");
    }
}
