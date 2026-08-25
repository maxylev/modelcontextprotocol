//! Semantic case catalog keyed by runtime MCP tool name.
//!
//! Every case carries the exact intended arguments (the model must return
//! them unchanged), an independent programmatic oracle over the MCP result
//! and fixture side effects, and coverage metadata. The catalog is the
//! single source of truth for what runs online; `assert_coverage` enforces
//! exact set equality with the runtime tool inventory and per-parameter
//! coverage, so added tools or parameters cannot be skipped silently.

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::model::{CallToolResult, ContentBlock, ResourceContents};

use super::harness::{A_TXT, BIG_PHRASE, FetchFixtureMode, memory_jsonl};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerId {
    Filesystem,
    Fetch,
    Memory,
    Shell,
}

#[derive(Clone)]
pub enum Oracle {
    /// `is_error` must not be `Some(true)`.
    Ok,
    /// `is_error == Some(true)` and the text contains the needle.
    ErrTextContains(&'static str),
    /// Text must contain every needle.
    TextContains(&'static [&'static str]),
    /// Text must not contain any needle.
    TextNotContains(&'static [&'static str]),
    /// Text must equal exactly.
    TextEquals(&'static str),
    /// Deterministic programmatic check with access to fixture state.
    Custom(OracleFn),
}

/// Oracle check function: (MCP result, fixture context) -> failure message.
pub type OracleFn = Arc<dyn Fn(&CallToolResult, &CaseCtx) -> Result<(), String> + Send + Sync>;

impl Oracle {
    fn custom(
        check: impl Fn(&CallToolResult, &CaseCtx) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        Oracle::Custom(Arc::new(check))
    }
}

/// Fixture-derived context available to every case oracle.
#[derive(Debug, Clone)]
pub struct CaseCtx {
    pub fs_root: PathBuf,
    pub mem_file: PathBuf,
    pub bin: &'static str,
    pub helper: String,
    pub shell_root: PathBuf,
    pub shell_work: PathBuf,
}

#[derive(Clone)]
pub struct Case {
    pub id: &'static str,
    pub server: ServerId,
    pub tool: &'static str,
    /// Exact intended arguments. Placeholders: `{base}` (fetch fixture URL),
    /// `{bin}` (main binary path), `{helper}` (this test binary path).
    pub args: serde_json::Value,
    pub oracle: Oracle,
    /// Respawn the server with the same configuration before this case runs
    /// (used for the memory persistence-across-restart oracle).
    pub respawn: bool,
    /// Execution group: groups run in ascending order; cases inside a group
    /// are independent and may run concurrently, groups with one case are
    /// serial. Stateful sequences are split into single-case groups.
    pub group: u8,
    /// Fetch fixture robots mode (only meaningful for fetch cases).
    pub fetch_mode: FetchFixtureMode,
    /// Why this case exists (coverage semantics), for the coverage doc.
    pub note: &'static str,
}

pub fn result_text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

// ---------------------------------------------------------------------------
// Oracle helpers
// ---------------------------------------------------------------------------

fn disk_equals(rel: &str, expected: &'static str) -> Oracle {
    let path = rel.to_string();
    Oracle::custom(move |_result, ctx| {
        let file = ctx.fs_root.join(&path);
        let actual = std::fs::read_to_string(&file)
            .map_err(|e| format!("read {} failed: {e}", file.display()))?;
        if actual == expected {
            Ok(())
        } else {
            Err(format!(
                "disk content of {} is {:?}, expected {:?}",
                file.display(),
                actual,
                expected
            ))
        }
    })
}

fn media_has_image(result: &CallToolResult, _ctx: &CaseCtx) -> Result<(), String> {
    let image = result
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::Image(i) => Some(i),
            _ => None,
        })
        .ok_or_else(|| "expected an image content block".to_string())?;
    if image.mime_type != "image/png" {
        return Err(format!("expected mime image/png, got {}", image.mime_type));
    }
    if image.data.is_empty() {
        return Err("image data is empty".to_string());
    }
    Ok(())
}

fn media_has_text_resource(result: &CallToolResult, _ctx: &CaseCtx) -> Result<(), String> {
    let resource = result
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::Resource(r) => Some(r),
            _ => None,
        })
        .ok_or_else(|| "expected a resource content block".to_string())?;
    match &resource.resource {
        ResourceContents::BlobResourceContents {
            mime_type, blob, ..
        } => {
            if mime_type.as_deref() != Some("text/plain") {
                return Err(format!("expected text/plain, got {mime_type:?}"));
            }
            let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, blob)
                .map_err(|e| format!("blob is not valid base64: {e}"))?;
            if decoded != A_TXT.as_bytes() {
                return Err("blob content differs from a.txt".to_string());
            }
            Ok(())
        }
        _ => Err("expected blob resource contents".to_string()),
    }
}

fn directory_tree_check(
    result: &CallToolResult,
    must_contain: &[&str],
    must_not_contain: &[&str],
) -> Result<(), String> {
    let text = result_text(result);
    let tree: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("directory_tree output is not JSON: {e}"))?;
    fn collect(tree: &serde_json::Value, out: &mut Vec<String>) {
        if let Some(entries) = tree.as_array() {
            for entry in entries {
                if let Some(name) = entry["name"].as_str() {
                    out.push(name.to_string());
                }
                if let Some(children) = entry["children"].as_array() {
                    collect(&serde_json::Value::Array(children.clone()), out);
                }
            }
        }
    }
    let mut names = Vec::new();
    collect(&tree, &mut names);
    for needle in must_contain {
        if !names.iter().any(|n| n == needle) {
            return Err(format!("tree is missing entry {needle:?}: {names:?}"));
        }
    }
    for needle in must_not_contain {
        if names.iter().any(|n| n == needle) {
            return Err(format!(
                "tree should not contain entry {needle:?}: {names:?}"
            ));
        }
    }
    Ok(())
}

fn structured(result: &CallToolResult) -> Result<serde_json::Value, String> {
    result
        .structured_content
        .clone()
        .ok_or_else(|| "expected structured content".to_string())
}

// ---------------------------------------------------------------------------
// Filesystem cases (serial sequence over one shared fixture)
// ---------------------------------------------------------------------------

fn fs_cases() -> Vec<Case> {
    let t = |text: &'static str| Oracle::TextEquals(text);
    let c = |text: &'static [&'static str]| Oracle::TextContains(text);
    vec![
        Case {
            id: "fs-001",
            server: ServerId::Filesystem,
            tool: "read_text_file",
            args: serde_json::json!({ "path": "a.txt" }),
            oracle: t(A_TXT),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "required path; head/tail omitted (defaults)",
        },
        Case {
            id: "fs-002",
            server: ServerId::Filesystem,
            tool: "read_text_file",
            args: serde_json::json!({ "path": "sub/head.txt", "head": 2 }),
            oracle: t("first\nsecond"),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "head provided",
        },
        Case {
            id: "fs-003",
            server: ServerId::Filesystem,
            tool: "read_text_file",
            args: serde_json::json!({ "path": "a.txt", "tail": 1 }),
            oracle: t("gamma"),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "tail provided",
        },
        Case {
            id: "fs-004",
            server: ServerId::Filesystem,
            tool: "read_file",
            args: serde_json::json!({ "path": "a.txt" }),
            oracle: t(A_TXT),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "deprecated alias; head/tail omitted",
        },
        Case {
            id: "fs-005",
            server: ServerId::Filesystem,
            tool: "read_file",
            args: serde_json::json!({ "path": "a.txt", "head": 1 }),
            oracle: t("alpha"),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "deprecated alias; head provided",
        },
        Case {
            id: "fs-006",
            server: ServerId::Filesystem,
            tool: "read_file",
            args: serde_json::json!({ "path": "a.txt", "tail": 2 }),
            oracle: t("beta\ngamma"),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "deprecated alias; tail provided",
        },
        Case {
            id: "fs-007",
            server: ServerId::Filesystem,
            tool: "read_media_file",
            args: serde_json::json!({ "path": "pixel.png" }),
            oracle: Oracle::custom(media_has_image),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "media content block",
        },
        Case {
            id: "fs-008",
            server: ServerId::Filesystem,
            tool: "read_media_file",
            args: serde_json::json!({ "path": "a.txt" }),
            oracle: Oracle::custom(media_has_text_resource),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "non-media file becomes embedded resource",
        },
        Case {
            id: "fs-009",
            server: ServerId::Filesystem,
            tool: "read_multiple_files",
            args: serde_json::json!({ "paths": [] }),
            oracle: Oracle::ErrTextContains("At least one file path"),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "empty list is rejected online",
        },
        Case {
            id: "fs-010",
            server: ServerId::Filesystem,
            tool: "read_multiple_files",
            args: serde_json::json!({ "paths": ["a.txt"] }),
            oracle: c(&["alpha"]),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "one path",
        },
        Case {
            id: "fs-011",
            server: ServerId::Filesystem,
            tool: "read_multiple_files",
            args: serde_json::json!({ "paths": ["a.txt", "docs/notes.md"] }),
            oracle: c(&["alpha", "# Title"]),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "multiple paths",
        },
        Case {
            id: "fs-012",
            server: ServerId::Filesystem,
            tool: "write_file",
            args: serde_json::json!({ "path": "w.txt", "content": "written content" }),
            oracle: disk_equals("w.txt", "written content"),
            respawn: false,
            group: 1,
            fetch_mode: FetchFixtureMode::Plain,
            note: "create + disk side effect",
        },
        Case {
            id: "fs-013",
            server: ServerId::Filesystem,
            tool: "write_file",
            args: serde_json::json!({ "path": "w.txt", "content": "overwritten" }),
            oracle: disk_equals("w.txt", "overwritten"),
            respawn: false,
            group: 2,
            fetch_mode: FetchFixtureMode::Plain,
            note: "overwrite behavior",
        },
        Case {
            id: "fs-014",
            server: ServerId::Filesystem,
            tool: "edit_file",
            args: serde_json::json!({
                "path": "w.txt",
                "edits": [{ "oldText": "overwritten", "newText": "EDITED" }],
                "dryRun": true
            }),
            oracle: Oracle::custom(|result, ctx| {
                // Preview must not modify the file on disk.
                let text = result_text(result);
                if !text.contains("+EDITED") {
                    return Err(format!("diff preview missing +EDITED: {text}"));
                }
                let actual = std::fs::read_to_string(ctx.fs_root.join("w.txt")).unwrap_or_default();
                if actual != "overwritten" {
                    return Err(format!("dry run modified the file: {actual:?}"));
                }
                Ok(())
            }),
            respawn: false,
            group: 3,
            fetch_mode: FetchFixtureMode::Plain,
            note: "dryRun true; edits with one item",
        },
        Case {
            id: "fs-015",
            server: ServerId::Filesystem,
            tool: "edit_file",
            args: serde_json::json!({
                "path": "w.txt",
                "edits": [{ "oldText": "overwritten", "newText": "FINAL" }],
                "dryRun": false
            }),
            oracle: disk_equals("w.txt", "FINAL"),
            respawn: false,
            group: 4,
            fetch_mode: FetchFixtureMode::Plain,
            note: "dryRun false applies the edit",
        },
        Case {
            id: "fs-016",
            server: ServerId::Filesystem,
            tool: "edit_file",
            args: serde_json::json!({
                "path": "w.txt",
                "edits": [
                    { "oldText": "FINAL", "newText": "v1" },
                    { "oldText": "v1", "newText": "v2" }
                ]
            }),
            oracle: disk_equals("w.txt", "v2"),
            respawn: false,
            group: 5,
            fetch_mode: FetchFixtureMode::Plain,
            note: "dryRun omitted; edits with two items",
        },
        Case {
            id: "fs-017",
            server: ServerId::Filesystem,
            tool: "create_directory",
            args: serde_json::json!({ "path": "d1/d2" }),
            oracle: Oracle::custom(|_result, ctx| {
                if ctx.fs_root.join("d1/d2").is_dir() {
                    Ok(())
                } else {
                    Err("d1/d2 was not created".to_string())
                }
            }),
            respawn: false,
            group: 6,
            fetch_mode: FetchFixtureMode::Plain,
            note: "nested directory creation",
        },
        Case {
            id: "fs-018",
            server: ServerId::Filesystem,
            tool: "move_file",
            args: serde_json::json!({ "source": "orig.txt", "destination": "d1/moved.txt" }),
            oracle: Oracle::custom(|_result, ctx| {
                if ctx.fs_root.join("orig.txt").exists() {
                    return Err("source still exists after move".to_string());
                }
                let moved = std::fs::read_to_string(ctx.fs_root.join("d1/moved.txt"))
                    .map_err(|e| format!("moved file missing: {e}"))?;
                if moved != "payload" {
                    return Err(format!("moved content {moved:?} != payload"));
                }
                Ok(())
            }),
            respawn: false,
            group: 7,
            fetch_mode: FetchFixtureMode::Plain,
            note: "rename/move with disk side effects",
        },
        Case {
            id: "fs-019",
            server: ServerId::Filesystem,
            tool: "list_directory",
            args: serde_json::json!({ "path": "" }),
            oracle: c(&["[FILE] a.txt", "[DIR] d1", "[DIR] docs", "[DIR] src"]),
            respawn: false,
            group: 8,
            fetch_mode: FetchFixtureMode::Plain,
            note: "file/dir markers",
        },
        Case {
            id: "fs-020",
            server: ServerId::Filesystem,
            tool: "list_directory_with_sizes",
            args: serde_json::json!({ "path": "" }),
            oracle: c(&["a.txt", "Total:"]),
            respawn: false,
            group: 8,
            fetch_mode: FetchFixtureMode::Plain,
            note: "sortBy omitted (default name)",
        },
        Case {
            id: "fs-021",
            server: ServerId::Filesystem,
            tool: "list_directory_with_sizes",
            args: serde_json::json!({ "path": "", "sortBy": "size" }),
            oracle: c(&["a.txt", "Total:"]),
            respawn: false,
            group: 8,
            fetch_mode: FetchFixtureMode::Plain,
            note: "sortBy provided (size)",
        },
        Case {
            id: "fs-022",
            server: ServerId::Filesystem,
            tool: "directory_tree",
            args: serde_json::json!({ "path": "" }),
            oracle: Oracle::custom(|result, _ctx| {
                directory_tree_check(result, &["src", "main.rs", "a.txt"], &[])
            }),
            respawn: false,
            group: 8,
            fetch_mode: FetchFixtureMode::Plain,
            note: "excludePatterns omitted",
        },
        Case {
            id: "fs-023",
            server: ServerId::Filesystem,
            tool: "directory_tree",
            args: serde_json::json!({ "path": "", "excludePatterns": ["**/*.rs"] }),
            oracle: Oracle::custom(|result, _ctx| {
                directory_tree_check(result, &["docs", "a.txt"], &["main.rs", "lib.rs"])
            }),
            respawn: false,
            group: 8,
            fetch_mode: FetchFixtureMode::Plain,
            note: "excludePatterns with one pattern",
        },
        Case {
            id: "fs-024",
            server: ServerId::Filesystem,
            tool: "directory_tree",
            args: serde_json::json!({ "path": "", "excludePatterns": ["src", "**/*.txt"] }),
            oracle: Oracle::custom(|result, _ctx| {
                directory_tree_check(result, &["docs", "sub"], &["src", "a.txt"])
            }),
            respawn: false,
            group: 8,
            fetch_mode: FetchFixtureMode::Plain,
            note: "excludePatterns with two patterns",
        },
        Case {
            id: "fs-025",
            server: ServerId::Filesystem,
            tool: "search_files",
            args: serde_json::json!({ "path": "", "pattern": "*.txt" }),
            oracle: Oracle::custom(|result, _ctx| {
                let text = result_text(result);
                if !text.contains("a.txt") {
                    return Err(format!("expected a.txt in search results: {text}"));
                }
                if text.contains("src/") {
                    return Err(format!("non-recursive search leaked into subdirs: {text}"));
                }
                Ok(())
            }),
            respawn: false,
            group: 8,
            fetch_mode: FetchFixtureMode::Plain,
            note: "excludePatterns omitted; current-dir glob",
        },
        Case {
            id: "fs-026",
            server: ServerId::Filesystem,
            tool: "search_files",
            args: serde_json::json!({
                "path": "",
                "pattern": "**/*.rs",
                "excludePatterns": ["**/sub/**"]
            }),
            oracle: Oracle::custom(|result, _ctx| {
                let text = result_text(result);
                for needle in ["src/main.rs", "src/lib.rs"] {
                    if !text.contains(needle) {
                        return Err(format!("missing {needle}: {text}"));
                    }
                }
                if text.contains("sub") {
                    return Err(format!("excluded pattern leaked: {text}"));
                }
                Ok(())
            }),
            respawn: false,
            group: 8,
            fetch_mode: FetchFixtureMode::Plain,
            note: "recursive glob with exclusion",
        },
        Case {
            id: "fs-027",
            server: ServerId::Filesystem,
            tool: "get_file_info",
            args: serde_json::json!({ "path": "a.txt" }),
            oracle: c(&["isFile: true", "isDirectory: false"]),
            respawn: false,
            group: 8,
            fetch_mode: FetchFixtureMode::Plain,
            note: "metadata fields",
        },
        Case {
            id: "fs-028",
            server: ServerId::Filesystem,
            tool: "list_allowed_directories",
            args: serde_json::json!({}),
            oracle: Oracle::custom(|result, ctx| {
                let canonical =
                    std::fs::canonicalize(&ctx.fs_root).unwrap_or_else(|_| ctx.fs_root.clone());
                let text = result_text(result);
                if text.contains(&canonical.display().to_string()) {
                    Ok(())
                } else {
                    Err(format!(
                        "expected allowed root {} in output",
                        canonical.display()
                    ))
                }
            }),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "zero-parameter tool",
        },
        Case {
            id: "fs-029",
            server: ServerId::Filesystem,
            tool: "read_text_file",
            args: serde_json::json!({ "path": "big.log", "head": 1 }),
            oracle: t("line 0"),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "large-file head read",
        },
    ]
}

// ---------------------------------------------------------------------------
// Memory cases (serial lifecycle over one unique temp JSONL)
// ---------------------------------------------------------------------------

fn mem_cases() -> Vec<Case> {
    let graph_has = |must_contain: &'static [&'static str],
                     must_not_contain: &'static [&'static str]| {
        Oracle::custom(move |result, _ctx| {
            let structured = structured(result)?;
            let text = serde_json::to_string(&structured).unwrap_or_default();
            for needle in must_contain {
                if !text.contains(needle) {
                    return Err(format!("graph missing {needle:?}: {text}"));
                }
            }
            for needle in must_not_contain {
                if text.contains(needle) {
                    return Err(format!("graph should not contain {needle:?}: {text}"));
                }
            }
            Ok(())
        })
    };
    // Entity-name-based check: search/open_nodes results legitimately include
    // relations whose endpoints are other entities, so only entity names are
    // constrained (search matches names/types/observations).
    let graph_entities = |must: &'static [&'static str], must_not: &'static [&'static str]| {
        Oracle::custom(move |result, _ctx| {
            let structured = structured(result)?;
            let names: Vec<String> = structured
                .get("entities")
                .and_then(serde_json::Value::as_array)
                .map(|entities| {
                    entities
                        .iter()
                        .filter_map(|e| e.get("name").and_then(serde_json::Value::as_str))
                        .map(String::from)
                        .collect()
                })
                .ok_or_else(|| "graph has no entities array".to_string())?;
            for needle in must {
                if !names.iter().any(|n| n == needle) {
                    return Err(format!("graph entities missing {needle:?}: {names:?}"));
                }
            }
            for needle in must_not {
                if names.iter().any(|n| n == needle) {
                    return Err(format!(
                        "graph entities should not contain {needle:?}: {names:?}"
                    ));
                }
            }
            Ok(())
        })
    };
    let jsonl_has = |must_contain: &'static [&'static str],
                     must_not_contain: &'static [&'static str]| {
        Oracle::custom(move |_result, ctx| {
            let file = memory_jsonl(&ctx.mem_file);
            for needle in must_contain {
                if !file.contains(needle) {
                    return Err(format!("JSONL missing {needle:?}: {file}"));
                }
            }
            for needle in must_not_contain {
                if file.contains(needle) {
                    return Err(format!("JSONL should not contain {needle:?}: {file}"));
                }
            }
            Ok(())
        })
    };
    vec![
        Case {
            id: "mem-000",
            server: ServerId::Memory,
            tool: "create_entities",
            args: serde_json::json!({ "entities": [] }),
            oracle: Oracle::Ok,
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "empty list is a no-op success",
        },
        Case {
            id: "mem-001",
            server: ServerId::Memory,
            tool: "create_entities",
            args: serde_json::json!({
                "entities": [{
                    "name": "carol",
                    "entityType": "pet",
                    "observations": ["meows"]
                }]
            }),
            oracle: jsonl_has(&["carol"], &[]),
            respawn: false,
            group: 1,
            fetch_mode: FetchFixtureMode::Plain,
            note: "one entity",
        },
        Case {
            id: "mem-002",
            server: ServerId::Memory,
            tool: "create_entities",
            args: serde_json::json!({
                "entities": [
                    { "name": "alice", "entityType": "person", "observations": [] },
                    { "name": "bob", "entityType": "person", "observations": ["plays guitar"] }
                ]
            }),
            oracle: jsonl_has(&["alice", "bob"], &[]),
            respawn: false,
            group: 2,
            fetch_mode: FetchFixtureMode::Plain,
            note: "multiple entities; empty observations list",
        },
        Case {
            id: "mem-003",
            server: ServerId::Memory,
            tool: "create_relations",
            args: serde_json::json!({ "relations": [] }),
            oracle: Oracle::Ok,
            respawn: false,
            group: 3,
            fetch_mode: FetchFixtureMode::Plain,
            note: "empty list is a no-op success",
        },
        Case {
            id: "mem-004",
            server: ServerId::Memory,
            tool: "create_relations",
            args: serde_json::json!({
                "relations": [{ "from": "alice", "to": "bob", "relationType": "knows" }]
            }),
            oracle: jsonl_has(&["knows"], &[]),
            respawn: false,
            group: 4,
            fetch_mode: FetchFixtureMode::Plain,
            note: "one relation",
        },
        Case {
            id: "mem-005",
            server: ServerId::Memory,
            tool: "create_relations",
            args: serde_json::json!({
                "relations": [
                    { "from": "bob", "to": "carol", "relationType": "owns" },
                    { "from": "carol", "to": "alice", "relationType": "likes" }
                ]
            }),
            oracle: jsonl_has(&["owns", "likes"], &[]),
            respawn: false,
            group: 5,
            fetch_mode: FetchFixtureMode::Plain,
            note: "multiple relations",
        },
        Case {
            id: "mem-006",
            server: ServerId::Memory,
            tool: "add_observations",
            args: serde_json::json!({
                "observations": [{
                    "entityName": "alice",
                    "contents": ["loves rust", "writes mcp"]
                }]
            }),
            oracle: jsonl_has(&["loves rust", "writes mcp"], &[]),
            respawn: false,
            group: 6,
            fetch_mode: FetchFixtureMode::Plain,
            note: "multiple observation contents",
        },
        Case {
            id: "mem-007",
            server: ServerId::Memory,
            tool: "read_graph",
            args: serde_json::json!({}),
            oracle: graph_has(&["alice", "bob", "carol", "knows"], &[]),
            respawn: false,
            group: 7,
            fetch_mode: FetchFixtureMode::Plain,
            note: "zero-parameter tool; full graph",
        },
        Case {
            id: "mem-008",
            server: ServerId::Memory,
            tool: "search_nodes",
            args: serde_json::json!({ "query": "rust" }),
            oracle: graph_entities(&["alice"], &["bob", "carol"]),
            respawn: false,
            group: 7,
            fetch_mode: FetchFixtureMode::Plain,
            note: "matching query",
        },
        Case {
            id: "mem-009",
            server: ServerId::Memory,
            tool: "search_nodes",
            args: serde_json::json!({ "query": "no-such-thing" }),
            oracle: graph_has(&[], &["alice", "bob", "carol"]),
            respawn: false,
            group: 7,
            fetch_mode: FetchFixtureMode::Plain,
            note: "non-matching query yields empty graph",
        },
        Case {
            id: "mem-010",
            server: ServerId::Memory,
            tool: "open_nodes",
            args: serde_json::json!({ "names": ["alice", "bob"] }),
            oracle: graph_entities(&["alice", "bob"], &["carol"]),
            respawn: false,
            group: 7,
            fetch_mode: FetchFixtureMode::Plain,
            note: "multiple names",
        },
        Case {
            id: "mem-011",
            server: ServerId::Memory,
            tool: "delete_observations",
            args: serde_json::json!({
                "deletions": [{
                    "entityName": "alice",
                    "observations": ["loves rust"]
                }]
            }),
            oracle: jsonl_has(&["writes mcp"], &["loves rust"]),
            respawn: false,
            group: 8,
            fetch_mode: FetchFixtureMode::Plain,
            note: "observation deletion persists",
        },
        Case {
            id: "mem-012",
            server: ServerId::Memory,
            tool: "delete_relations",
            args: serde_json::json!({
                "relations": [{ "from": "carol", "to": "alice", "relationType": "likes" }]
            }),
            oracle: jsonl_has(&["knows", "owns"], &["likes"]),
            respawn: false,
            group: 9,
            fetch_mode: FetchFixtureMode::Plain,
            note: "relation deletion persists",
        },
        Case {
            id: "mem-013",
            server: ServerId::Memory,
            tool: "delete_entities",
            args: serde_json::json!({ "entityNames": ["bob", "carol"] }),
            oracle: jsonl_has(&["alice", "writes mcp"], &["bob", "carol"]),
            respawn: false,
            group: 10,
            fetch_mode: FetchFixtureMode::Plain,
            note: "multiple entity deletion; relations pruned",
        },
        Case {
            id: "mem-014",
            server: ServerId::Memory,
            tool: "read_graph",
            args: serde_json::json!({}),
            oracle: graph_has(&["alice", "writes mcp"], &["bob", "carol", "knows"]),
            respawn: false,
            group: 11,
            fetch_mode: FetchFixtureMode::Plain,
            note: "final state after lifecycle",
        },
        Case {
            id: "mem-015",
            server: ServerId::Memory,
            tool: "read_graph",
            args: serde_json::json!({}),
            oracle: graph_has(&["alice", "writes mcp"], &["bob", "carol"]),
            respawn: true,
            group: 12,
            fetch_mode: FetchFixtureMode::Plain,
            note: "persistence across server restart",
        },
    ]
}

// ---------------------------------------------------------------------------
// Fetch cases (each with its own deterministic local fixture)
// ---------------------------------------------------------------------------

fn fetch_cases() -> Vec<Case> {
    // The /big fixture serves the phrase wrapped in an HTML document; raw
    // mode returns the full page, so oracles slice the wrapped page.
    let page = Arc::new(format!(
        "<html><body><article>{}</article></body></html>",
        BIG_PHRASE.repeat(20)
    ));
    let truncation_hint = |start: usize| {
        format!(
            "\n\n<error>Content truncated. Call the fetch tool with a start_index of \
             {start} to get more content.</error>"
        )
    };
    vec![
        Case {
            id: "ft-001",
            server: ServerId::Fetch,
            tool: "fetch",
            args: serde_json::json!({ "url": "{base}/page" }),
            oracle: Oracle::TextContains(&["Hello World", "test paragraph"]),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "max_length/start_index/raw omitted (defaults); html→markdown",
        },
        Case {
            id: "ft-002",
            server: ServerId::Fetch,
            tool: "fetch",
            args: serde_json::json!({ "url": "{base}/page", "raw": true }),
            oracle: Oracle::TextContains(&["<h1>Hello World</h1>"]),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "raw true keeps markup",
        },
        Case {
            id: "ft-003",
            server: ServerId::Fetch,
            tool: "fetch",
            args: serde_json::json!({ "url": "{base}/page", "raw": false }),
            oracle: Oracle::TextNotContains(&["<h1>"]),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "raw false explicitly simplifies to markdown",
        },
        Case {
            id: "ft-004",
            server: ServerId::Fetch,
            tool: "fetch",
            args: serde_json::json!({ "url": "{base}/plain.txt" }),
            oracle: Oracle::TextContains(&["plain text content"]),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "non-HTML content",
        },
        Case {
            id: "ft-005",
            server: ServerId::Fetch,
            tool: "fetch",
            args: serde_json::json!({ "url": "{base}/big", "raw": true, "max_length": 120 }),
            oracle: Oracle::custom({
                let page = Arc::clone(&page);
                move |result, _ctx| {
                    let text = result_text(result);
                    let expected = format!("{}{}", &page[..120], truncation_hint(120));
                    if text.ends_with(&expected) {
                        Ok(())
                    } else {
                        Err(format!(
                            "truncated tail mismatch (text {} chars)",
                            text.chars().count()
                        ))
                    }
                }
            }),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "max_length truncation with continuation hint",
        },
        Case {
            id: "ft-006",
            server: ServerId::Fetch,
            tool: "fetch",
            args: serde_json::json!({
                "url": "{base}/big",
                "raw": true,
                "start_index": 100,
                "max_length": 820
            }),
            oracle: Oracle::custom({
                let page = Arc::clone(&page);
                move |result, _ctx| {
                    let text = result_text(result);
                    let expected = format!("{}{}", &page[100..920], truncation_hint(920));
                    if text.ends_with(&expected) {
                        Ok(())
                    } else {
                        Err(format!(
                            "start_index slice mismatch (text {} chars)",
                            text.chars().count()
                        ))
                    }
                }
            }),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "start_index resume exactly to the end",
        },
        Case {
            id: "ft-007",
            server: ServerId::Fetch,
            tool: "fetch",
            args: serde_json::json!({ "url": "{base}/echo-ua", "raw": true }),
            oracle: Oracle::TextContains(&["Mozilla/5.0 (Windows NT 10.0; Win64; x64)"]),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "user-agent observation",
        },
        Case {
            id: "ft-008",
            server: ServerId::Fetch,
            tool: "fetch",
            args: serde_json::json!({ "url": "{base}/page" }),
            oracle: Oracle::ErrTextContains("robots.txt"),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::DisallowAll,
            note: "robots disallow is rejected",
        },
        Case {
            id: "ft-009",
            server: ServerId::Fetch,
            tool: "fetch",
            args: serde_json::json!({ "url": "{base}/page" }),
            oracle: Oracle::TextContains(&["Hello World"]),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::AllowAll,
            note: "robots allow proceeds",
        },
        Case {
            id: "ft-010",
            server: ServerId::Fetch,
            tool: "fetch",
            args: serde_json::json!({ "url": "{base}/page" }),
            oracle: Oracle::TextContains(&["Hello World"]),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "robots.txt missing (404) proceeds",
        },
        Case {
            id: "ft-011",
            server: ServerId::Fetch,
            tool: "fetch",
            args: serde_json::json!({ "url": "{base}/missing" }),
            oracle: Oracle::ErrTextContains("status code 404"),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "HTTP error surfaces as tool error",
        },
    ]
}

// ---------------------------------------------------------------------------
// Shell cases (isolated temp cwd; helper = this test binary)
// ---------------------------------------------------------------------------

fn helper_argv(mode: &str, payload: &[&str]) -> serde_json::Value {
    let mut args = vec!["--nocapture", "--exact", "e2e_helper_command", mode];
    args.extend(payload);
    serde_json::Value::Array(
        args.iter()
            .map(|a| serde_json::Value::String(a.to_string()))
            .collect(),
    )
}

fn shell_cases() -> Vec<Case> {
    vec![
        Case {
            id: "sh-001",
            server: ServerId::Shell,
            tool: "execute_command",
            args: serde_json::json!({ "program": "{bin}", "args": ["--version"] }),
            oracle: Oracle::custom(|result, _ctx| {
                let out = structured(result)?;
                let stdout = out["stdout"].as_str().unwrap_or_default();
                if out["exit_code"] == serde_json::json!(0)
                    && stdout.contains("modelcontextprotocol")
                {
                    Ok(())
                } else {
                    Err(format!("--version case: {out}"))
                }
            }),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "argv passthrough, stdout, exit 0; cwd omitted (default)",
        },
        Case {
            id: "sh-002",
            server: ServerId::Shell,
            tool: "execute_command",
            args: serde_json::json!({ "program": "{bin}" }),
            oracle: Oracle::custom(|result, _ctx| {
                let out = structured(result)?;
                let stderr = out["stderr"].as_str().unwrap_or_default();
                if out["exit_code"] != serde_json::json!(0) && stderr.contains("Usage") {
                    Ok(())
                } else {
                    Err(format!("no-args case: {out}"))
                }
            }),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "args omitted (empty default); nonzero exit + stderr",
        },
        Case {
            id: "sh-003",
            server: ServerId::Shell,
            tool: "execute_command",
            args: serde_json::json!({ "program": "{bin}", "args": ["filesystem"] }),
            oracle: Oracle::custom(|result, _ctx| {
                let out = structured(result)?;
                let stderr = out["stderr"].as_str().unwrap_or_default();
                if out["exit_code"] != serde_json::json!(0) && stderr.contains("filesystem") {
                    Ok(())
                } else {
                    Err(format!("usage case: {out}"))
                }
            }),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "argv changes behavior; nonzero + stderr",
        },
        Case {
            id: "sh-004",
            server: ServerId::Shell,
            tool: "execute_command",
            args: serde_json::json!({
                "program": "{helper}",
                "args": helper_argv("echo", &["hello", "from", "helper"])
            }),
            oracle: Oracle::custom(|result, _ctx| {
                let out = structured(result)?;
                let stdout = out["stdout"].as_str().unwrap_or_default();
                if out["exit_code"] == serde_json::json!(0)
                    && ["hello", "from", "helper"]
                        .iter()
                        .all(|w| stdout.contains(w))
                {
                    Ok(())
                } else {
                    Err(format!("echo case: {out}"))
                }
            }),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "argv preserved exactly (helper echo)",
        },
        Case {
            id: "sh-005",
            server: ServerId::Shell,
            tool: "execute_command",
            args: serde_json::json!({
                "program": "{helper}",
                "args": helper_argv("stderr", &[])
            }),
            oracle: Oracle::custom(|result, _ctx| {
                let out = structured(result)?;
                let stderr = out["stderr"].as_str().unwrap_or_default();
                if stderr.contains("helper stderr line") {
                    Ok(())
                } else {
                    Err(format!("stderr case: {out}"))
                }
            }),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "stderr capture",
        },
        Case {
            id: "sh-006",
            server: ServerId::Shell,
            tool: "execute_command",
            args: serde_json::json!({
                "program": "{helper}",
                "args": helper_argv("exit", &["7"])
            }),
            oracle: Oracle::custom(|result, _ctx| {
                let out = structured(result)?;
                if out["exit_code"] == serde_json::json!(7) {
                    Ok(())
                } else {
                    Err(format!("exit case: {out}"))
                }
            }),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "nonzero exit code passthrough",
        },
        Case {
            id: "sh-007",
            server: ServerId::Shell,
            tool: "execute_command",
            args: serde_json::json!({
                "program": "{helper}",
                "args": helper_argv("cwd", &[]),
                "cwd": "work"
            }),
            oracle: Oracle::custom(|result, ctx| {
                let out = structured(result)?;
                let stdout = out["stdout"].as_str().unwrap_or_default();
                let canonical = std::fs::canonicalize(&ctx.shell_work)
                    .unwrap_or_else(|_| ctx.shell_work.clone());
                if stdout.contains(&canonical.display().to_string()) {
                    Ok(())
                } else {
                    Err(format!(
                        "cwd case: expected {} in {out}",
                        canonical.display()
                    ))
                }
            }),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "cwd provided (relative to allowed root)",
        },
        Case {
            id: "sh-008",
            server: ServerId::Shell,
            tool: "execute_command",
            args: serde_json::json!({
                "program": "{helper}",
                "args": helper_argv("cwd", &[])
            }),
            oracle: Oracle::custom(|result, ctx| {
                let out = structured(result)?;
                let stdout = out["stdout"].as_str().unwrap_or_default();
                let canonical = std::fs::canonicalize(&ctx.shell_root)
                    .unwrap_or_else(|_| ctx.shell_root.clone());
                if stdout.contains(&canonical.display().to_string()) {
                    Ok(())
                } else {
                    Err(format!(
                        "default cwd case: expected {} in {out}",
                        canonical.display()
                    ))
                }
            }),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "cwd omitted (defaults to first allowed dir)",
        },
        Case {
            id: "sh-009",
            server: ServerId::Shell,
            tool: "execute_command",
            args: serde_json::json!({
                "program": "{helper}",
                "args": helper_argv("sleep", &["10000"]),
                "timeout_ms": 250
            }),
            oracle: Oracle::custom(|result, _ctx| {
                let out = structured(result)?;
                if out["timed_out"] == serde_json::json!(true) && out["exit_code"].is_null() {
                    Ok(())
                } else {
                    Err(format!("timeout case: {out}"))
                }
            }),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "timeout_ms provided; process terminated",
        },
        Case {
            id: "sh-010",
            server: ServerId::Shell,
            tool: "execute_command",
            args: serde_json::json!({
                "program": "{helper}",
                "args": helper_argv("big", &[])
            }),
            oracle: Oracle::custom(|result, _ctx| {
                let out = structured(result)?;
                let stdout = out["stdout"].as_str().unwrap_or_default();
                if out["stdout_truncated"] == serde_json::json!(true)
                    && stdout.len() == 1024 * 1024
                    && out["exit_code"] == serde_json::json!(0)
                {
                    Ok(())
                } else {
                    Err(format!("truncation case: {out}"))
                }
            }),
            respawn: false,
            group: 0,
            fetch_mode: FetchFixtureMode::Plain,
            note: "stdout truncated at 1 MiB; timeout_ms omitted (default)",
        },
    ]
}

// ---------------------------------------------------------------------------
// Catalog and coverage assertions
// ---------------------------------------------------------------------------

pub fn catalog() -> Vec<Case> {
    let mut cases = Vec::new();
    cases.extend(fs_cases());
    cases.extend(mem_cases());
    cases.extend(fetch_cases());
    cases.extend(shell_cases());
    cases
}

pub fn cases_for(server: ServerId) -> Vec<Case> {
    catalog()
        .into_iter()
        .filter(|c| c.server == server)
        .collect()
}

pub fn server_name(server: ServerId) -> &'static str {
    match server {
        ServerId::Filesystem => "filesystem",
        ServerId::Fetch => "fetch",
        ServerId::Memory => "memory",
        ServerId::Shell => "shell",
    }
}

/// Assert that the catalog matches the runtime inventory exactly and that
/// every runtime parameter is covered by at least one online case (provided
/// for every parameter; additionally omitted for optional parameters).
///
/// `runtime` maps tool name → (parameter names, required parameter names).
/// Returns a list of violations (never panics itself).
pub fn assert_coverage(
    server: ServerId,
    runtime: &[(String, Vec<String>, Vec<String>)],
    catalog_cases: &[Case],
) -> Vec<String> {
    let mut violations = Vec::new();

    let catalog_names: std::collections::BTreeSet<&str> =
        catalog_cases.iter().map(|c| c.tool).collect();
    let runtime_names: std::collections::BTreeSet<&str> =
        runtime.iter().map(|(name, _, _)| name.as_str()).collect();

    if catalog_names != runtime_names {
        let missing_from_catalog: Vec<&str> =
            runtime_names.difference(&catalog_names).copied().collect();
        let unknown_in_catalog: Vec<&str> =
            catalog_names.difference(&runtime_names).copied().collect();
        violations.push(format!(
            "{:?}: catalog/runtime tool set mismatch: catalog-only={unknown_in_catalog:?}, \
             runtime-only={missing_from_catalog:?}",
            server
        ));
    }

    for (name, params, required) in runtime {
        let cases: Vec<&Case> = catalog_cases.iter().filter(|c| c.tool == name).collect();
        for param in params {
            let provided = cases.iter().any(|c| c.args.get(param).is_some());
            if !provided {
                violations.push(format!(
                    "{:?} tool {name:?}: parameter {param:?} is never provided in an online \
                     case",
                    server
                ));
            }
            let optional = !required.contains(param);
            if optional {
                let omitted = cases.iter().any(|c| c.args.get(param).is_none());
                if !omitted {
                    violations.push(format!(
                        "{:?} tool {name:?}: optional parameter {param:?} is never omitted in \
                         an online case",
                        server
                    ));
                }
            }
        }
    }

    let mut seen_ids = std::collections::BTreeSet::new();
    for case in catalog_cases {
        if !seen_ids.insert(case.id) {
            violations.push(format!("duplicate case id {}", case.id));
        }
        if !runtime_names.contains(case.tool) {
            violations.push(format!(
                "case {} references unknown tool {:?}",
                case.id, case.tool
            ));
        }
    }
    violations
}
