use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::support::AccessControl;

/// Build a glob set with `minimatch`-compatible semantics:
/// `*` does not cross `/`, `**` does. globset matches dotfiles by default,
/// which mirrors `minimatch` with `{ dot: true }`.
fn build_glob_set(patterns: &[String]) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .backslash_escape(false)
            .build()
            .map_err(|e| format!("Invalid pattern {pattern:?}: {e}"))?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|e| format!("Failed to build pattern set: {e}"))
}

/// Recursively search `root` for entries whose path (relative to `root`,
/// using `/` separators) matches `pattern`. `exclude_patterns` are matched the
/// same way and remove matches. Entries that do not validate against
/// `access` (e.g. symlinks escaping the allowed directories) are skipped.
/// Returns full paths to the matches.
pub async fn search_files(
    root: &Path,
    pattern: &str,
    exclude_patterns: &[String],
    access: &AccessControl,
) -> Result<Vec<PathBuf>, String> {
    let matcher = build_glob_set(&[pattern.to_string()])?;
    let excludes = if exclude_patterns.is_empty() {
        None
    } else {
        Some(build_glob_set(exclude_patterns)?)
    };

    let mut results = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(current) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&current)
            .await
            .map_err(|e| format!("Failed to read directory {}: {e}", current.display()))?;

        while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
            let path = entry.path();
            let is_dir = entry.file_type().await.map_err(|e| e.to_string())?.is_dir();

            // Skip anything that resolves outside the allowed roots.
            if access.validate_path(&path.to_string_lossy()).await.is_err() {
                continue;
            }

            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");

            if let Some(excludes) = &excludes
                && excludes.is_match(&relative)
            {
                continue;
            }

            if matcher.is_match(&relative) {
                results.push(path.clone());
            }

            if is_dir {
                stack.push(path);
            }
        }
    }

    Ok(results)
}

/// A node in the JSON directory tree produced by `directory_tree`.
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub struct TreeEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<TreeEntry>>,
}

/// Maximum number of entries a `directory_tree` result may contain. Building
/// stops once the budget is exhausted and a truncation marker entry is
/// appended, bounding the work done to build the tree so a single call
/// cannot overflow the context window. The serialized JSON is additionally
/// capped at [`crate::support::MAX_TOOL_RESULT_BYTES`] by the tool handler,
/// which bounds the output size even when entry names are very long.
pub const MAX_TREE_ENTRIES: usize = 1024;

/// Recursively build a tree of a directory, honoring exclude patterns.
///
/// Excludes are matched against the path relative to `root` the same way the
/// reference server does: patterns containing `*` are plain globs; patterns
/// without `*` also match any path that ends with the pattern or contains it
/// anywhere in its path.
///
/// At most [`MAX_TREE_ENTRIES`] entries are returned; when that budget is
/// exhausted the walk stops and a single marker entry with kind `"truncated"`
/// is embedded where the limit was hit, keeping the output valid JSON.
pub async fn directory_tree(
    root: &Path,
    exclude_patterns: &[String],
    access: &AccessControl,
) -> Result<Vec<TreeEntry>, String> {
    let mut budget = MAX_TREE_ENTRIES;
    let mut truncated = false;
    build_tree(
        root,
        root,
        exclude_patterns,
        access,
        &mut budget,
        &mut truncated,
    )
    .await
}

async fn build_tree(
    root: &Path,
    current: &Path,
    exclude_patterns: &[String],
    access: &AccessControl,
    budget: &mut usize,
    truncated: &mut bool,
) -> Result<Vec<TreeEntry>, String> {
    let mut entries = tokio::fs::read_dir(current)
        .await
        .map_err(|e| format!("Failed to read directory {}: {e}", current.display()))?;
    let mut result = Vec::new();

    while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
        if *budget == 0 {
            *truncated = true;
            result.push(TreeEntry {
                name: format!(
                    "... (tree truncated at {MAX_TREE_ENTRIES} entries; pass excludePatterns \
                     to exclude e.g. node_modules, or use list_directory for a single directory)"
                ),
                kind: "truncated",
                children: None,
            });
            break;
        }
        let path = entry.path();
        if access.validate_path(&path.to_string_lossy()).await.is_err() {
            continue;
        }
        let is_dir = entry.file_type().await.map_err(|e| e.to_string())?.is_dir();

        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if excluded(&rel_str, exclude_patterns) {
            continue;
        }

        *budget -= 1;
        let mut node = TreeEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind: if is_dir { "directory" } else { "file" },
            children: None,
        };

        if is_dir {
            let children = Box::pin(build_tree(
                root,
                &path,
                exclude_patterns,
                access,
                budget,
                truncated,
            ))
            .await?;
            node.children = Some(children);
            if *truncated {
                result.push(node);
                break;
            }
        }

        result.push(node);
    }

    Ok(result)
}

fn excluded(relative: &str, exclude_patterns: &[String]) -> bool {
    exclude_patterns.iter().any(|pattern| {
        if pattern.contains('*') {
            matches_glob(relative, pattern)
        } else {
            matches_glob(relative, pattern)
                || matches_glob(relative, &format!("**/{pattern}"))
                || matches_glob(relative, &format!("**/{pattern}/**"))
        }
    })
}

fn matches_glob(relative: &str, pattern: &str) -> bool {
    let Ok(set) = build_glob_set(&[pattern.to_string()]) else {
        return false;
    };
    set.is_match(relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_star_does_not_cross_separator() {
        assert!(matches_glob("file.rs", "*.rs"));
        assert!(matches_glob("a/b.rs", "**/*.rs"));
        assert!(!matches_glob("a/b.rs", "*.rs"));
    }

    #[test]
    fn glob_matches_dotfiles() {
        assert!(matches_glob(".hidden", ".*"));
        assert!(matches_glob("a/.hidden", "**/.*"));
    }

    #[test]
    fn excluded_patterns_without_star() {
        assert!(excluded("node_modules", &["node_modules".into()]));
        assert!(excluded("a/node_modules", &["node_modules".into()]));
        assert!(excluded("node_modules/pkg", &["node_modules".into()]));
        assert!(excluded("a/node_modules/pkg", &["node_modules".into()]));
        assert!(!excluded("a/nodemodules", &["node_modules".into()]));
    }

    #[test]
    fn excluded_patterns_with_star() {
        assert!(excluded("a/b.rs", &["a/*.rs".into()]));
        assert!(!excluded("b/a.rs", &["a/*.rs".into()]));
    }
}
