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

/// Maximum size in bytes of a single tool result's text content. Tool
/// outputs that exceed this are truncated (see [`truncate_text`]) so a
/// single call — e.g. `directory_tree` on a huge tree or `read_text_file`
/// on a large file — cannot overflow the client's context window.
pub const MAX_TOOL_RESULT_BYTES: usize = 64 * 1024;

/// Truncate `text` so it fits within `max_bytes` bytes, cutting on a UTF-8
/// character boundary. When anything is cut, `notice` is appended after the
/// truncated content so callers can explain what was dropped and how to
/// retrieve the rest. Returns `text` unchanged when it already fits.
pub fn truncate_text(text: &str, max_bytes: usize, notice: &str) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = String::with_capacity(end + notice.len());
    truncated.push_str(&text[..end]);
    truncated.push_str(notice);
    truncated
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

#[cfg(test)]
mod truncate_tests {
    use super::*;

    #[test]
    fn short_text_is_returned_unchanged() {
        assert_eq!(truncate_text("hello", 10, "…"), "hello");
        assert_eq!(truncate_text("hello", 5, "…"), "hello");
    }

    #[test]
    fn long_text_is_cut_on_char_boundary_with_notice() {
        let result = truncate_text("hello world", 8, "…[cut]");
        assert_eq!(result, "hello wo…[cut]");
    }

    #[test]
    fn multibyte_characters_are_not_split() {
        let text = "héllo wörld"; // 'é' and 'ö' are 2-byte UTF-8
        // A limit landing mid-'é' (bytes 1..=2) must backtrack to byte 1.
        let result = truncate_text(text, 2, "…");
        assert_eq!(result, "h…");
        assert!(result.is_char_boundary(result.len()));
        // Limits on a boundary cut cleanly.
        let result = truncate_text(text, 5, "…");
        assert_eq!(result, "héll…");
    }

    #[test]
    fn empty_text_and_zero_limit() {
        assert_eq!(truncate_text("", 64, "…"), "");
        assert_eq!(truncate_text("abc", 0, "…"), "…");
    }
}
