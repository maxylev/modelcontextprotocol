use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

/// Lexically normalize a path: collapses `.`, resolves `..` where possible,
/// and strips redundant separators. Does not touch the filesystem.
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let at_root = out
                    .components()
                    .next_back()
                    .is_some_and(|c| matches!(c, Component::RootDir | Component::Prefix(_)));
                if at_root {
                    // `..` at the root is a no-op.
                } else if out.pop() {
                    // resolved a previous component
                } else {
                    // Leading `..` for relative paths is preserved.
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Expand a leading `~` to the user's home directory.
pub fn expand_home(path: &Path) -> PathBuf {
    if path == Path::new("~") {
        return home_dir().unwrap_or_else(|| path.to_path_buf());
    }
    if let (Ok(rest), Some(home)) = (path.strip_prefix("~"), home_dir()) {
        return home.join(rest);
    }
    path.to_path_buf()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// True when `path` is lexically inside at least one of `roots`.
///
/// Component-wise so that `/foo/bar` is not considered inside `/foo/bar2`.
pub fn is_within(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

/// Controls which directories the filesystem server may touch.
///
/// Directories are stored both as given (lexically normalized) and as
/// canonicalized. This mirrors the reference TypeScript server and fixes the
/// macOS `/tmp -> /private/tmp` symlink mismatch: a path requested as
/// `/tmp/x` still validates when the allowed root was given as
/// `/private/tmp`.
#[derive(Debug, Clone, Default)]
pub struct AccessControl {
    /// Allowed roots: originals plus their canonicalized variants.
    roots: Vec<PathBuf>,
}

impl AccessControl {
    /// Build access control from command-line arguments.
    ///
    /// Dirs that do not exist or are not directories are skipped with a
    /// warning. Returns an error when no usable directory remains.
    pub fn from_args(dirs: &[PathBuf]) -> Result<Self, String> {
        if dirs.is_empty() {
            return Err(
                "no allowed directories provided - pass at least one directory".to_string(),
            );
        }

        let mut roots: Vec<PathBuf> = Vec::new();
        let mut specified = 0usize;

        for dir in dirs {
            let expanded = expand_home(dir);
            let absolute = if expanded.is_absolute() {
                expanded
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(expanded)
            };
            let normalized = normalize_path(&absolute);
            specified += 1;

            let canonical = std::fs::canonicalize(&normalized);
            match canonical {
                Ok(real) => {
                    let meta = std::fs::metadata(&real);
                    if let Ok(meta) = meta {
                        if meta.is_dir() {
                            if real != normalized {
                                roots.push(normalized.clone());
                            }
                            roots.push(real);
                        } else {
                            eprintln!(
                                "Warning: {} is not a directory, skipping",
                                normalized.display()
                            );
                        }
                    } else {
                        eprintln!(
                            "Warning: Cannot access directory {}, skipping",
                            normalized.display()
                        );
                    }
                }
                Err(_) => {
                    eprintln!(
                        "Warning: Cannot access directory {}, skipping",
                        normalized.display()
                    );
                }
            }
        }

        if roots.is_empty() {
            return Err(format!(
                "none of the {specified} specified directorie(s) are accessible - \
                 the server cannot operate without at least one allowed directory"
            ));
        }
        Ok(Self { roots })
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Resolve and verify a user-supplied path against the allowed roots.
    ///
    /// Returns the canonical path for existing entries, the absolute path for
    /// entries that do not exist yet (their parent must exist and be allowed),
    /// or an error describing why access is denied.
    pub async fn validate_path(&self, requested: &str) -> Result<PathBuf, String> {
        if self.roots.is_empty() {
            return Err("server has no allowed directories".to_string());
        }

        let expanded = expand_home(Path::new(requested));
        let absolute = if expanded.is_absolute() {
            expanded
        } else {
            // Relative paths resolve against the first allowed directory.
            self.roots[0].join(expanded)
        };
        let normalized = normalize_path(&absolute);

        if !is_within(&normalized, &self.roots) {
            return Err(format!(
                "Access denied - path outside allowed directories: {} not in {}",
                normalized.display(),
                self.display_roots()
            ));
        }

        match tokio::fs::canonicalize(&normalized).await {
            Ok(real) => {
                if !is_within(&real, &self.roots) {
                    return Err(format!(
                        "Access denied - symlink target outside allowed directories: {} not in {}",
                        real.display(),
                        self.display_roots()
                    ));
                }
                Ok(real)
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                // New files/directories: walk up to the deepest existing
                // ancestor and verify it resolves inside an allowed root.
                // This keeps symlink protection (a symlinked ancestor must
                // still land within the allowed roots) while allowing whole
                // trees to be created at once.
                let mut ancestor = normalized.as_path();
                loop {
                    match tokio::fs::canonicalize(ancestor).await {
                        Ok(real) => {
                            if !is_within(&real, &self.roots) {
                                return Err(format!(
                                    "Access denied - parent directory outside allowed \
                                     directories: {} not in {}",
                                    real.display(),
                                    self.display_roots()
                                ));
                            }
                            return Ok(normalized);
                        }
                        Err(e) if e.kind() == ErrorKind::NotFound => match ancestor.parent() {
                            Some(parent) if parent != ancestor => ancestor = parent,
                            _ => {
                                return Err(format!(
                                    "Parent directory does not exist: {}",
                                    normalized.display()
                                ));
                            }
                        },
                        Err(e) => {
                            return Err(format!("Failed to access {}: {e}", normalized.display()));
                        }
                    }
                }
            }
            Err(e) => Err(format!("Failed to access {}: {e}", normalized.display())),
        }
    }

    fn display_roots(&self) -> String {
        self.roots
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_dots() {
        assert_eq!(
            normalize_path(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
        assert_eq!(normalize_path(Path::new("/a/././b")), PathBuf::from("/a/b"));
    }

    #[test]
    fn normalize_keeps_leading_parents_for_relative_paths() {
        assert_eq!(normalize_path(Path::new("../a/b")), PathBuf::from("../a/b"));
        assert_eq!(
            normalize_path(Path::new("a/../../b")),
            PathBuf::from("../b")
        );
    }

    #[test]
    fn is_within_is_component_aware() {
        let roots = vec![PathBuf::from("/foo/bar")];
        assert!(is_within(Path::new("/foo/bar"), &roots));
        assert!(is_within(Path::new("/foo/bar/baz"), &roots));
        assert!(!is_within(Path::new("/foo/bar2"), &roots));
        assert!(!is_within(Path::new("/foo"), &roots));
    }

    #[test]
    fn expand_home_replaces_tilde() {
        let home = home_dir().expect("HOME set in test env");
        assert_eq!(expand_home(Path::new("~")), home);
        assert_eq!(expand_home(Path::new("~/doc.txt")), home.join("doc.txt"));
        assert_eq!(expand_home(Path::new("/abs")), PathBuf::from("/abs"));
        assert_eq!(
            expand_home(Path::new("rel/path")),
            PathBuf::from("rel/path")
        );
    }
}
