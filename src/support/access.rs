use std::ffi::OsString;
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
    home_dir_from(|key| std::env::var_os(key))
}

/// Resolve the home directory from an environment lookup, trying the
/// platform conventions in order: `HOME`, then `USERPROFILE`, then
/// `HOMEDRIVE` + `HOMEPATH` (both present and non-empty). Standard library
/// only, so it works on Unix and Windows CI runners alike.
fn home_dir_from(mut get: impl FnMut(&str) -> Option<OsString>) -> Option<PathBuf> {
    if let Some(home) = get("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = get("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    let (Some(drive), Some(path)) = (get("HOMEDRIVE"), get("HOMEPATH")) else {
        return None;
    };
    if drive.is_empty() || path.is_empty() {
        return None;
    }
    Some(PathBuf::from(drive).join(path))
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
        // Resolves via HOME on Unix, USERPROFILE on Windows runners.
        let home = home_dir().expect("home dir resolvable in test env");
        assert_eq!(expand_home(Path::new("~")), home);
        assert_eq!(expand_home(Path::new("~/doc.txt")), home.join("doc.txt"));
        assert_eq!(expand_home(Path::new("/abs")), PathBuf::from("/abs"));
        assert_eq!(
            expand_home(Path::new("rel/path")),
            PathBuf::from("rel/path")
        );
    }

    // The lookups below use an injected env closure instead of touching the
    // process-global environment, so tests can run in parallel safely.
    fn env_of<'a>(entries: &'a [(&'a str, &'a str)]) -> impl FnMut(&str) -> Option<OsString> + 'a {
        let entries: Vec<(&'a str, &'a str)> = entries.to_vec();
        move |key| {
            entries
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(*v))
        }
    }

    #[test]
    fn home_dir_prefers_home() {
        let get = env_of(&[("HOME", "/home/usr"), ("USERPROFILE", "C:\\Users\\bob")]);
        assert_eq!(home_dir_from(get), Some(PathBuf::from("/home/usr")));
    }

    #[test]
    fn home_dir_falls_back_to_userprofile() {
        let get = env_of(&[("USERPROFILE", "C:\\Users\\bob")]);
        assert_eq!(home_dir_from(get), Some(PathBuf::from("C:\\Users\\bob")));
    }

    #[test]
    fn home_dir_joins_homedrive_and_homepath() {
        let get = env_of(&[("HOMEDRIVE", "C:"), ("HOMEPATH", "\\Users\\bob")]);
        // `\` is a separator on Windows but a plain character on Unix, so
        // the joined result differs per platform.
        #[cfg(windows)]
        let expected = PathBuf::from("C:\\Users\\bob");
        #[cfg(not(windows))]
        let expected = PathBuf::from("C:/\\Users\\bob");
        assert_eq!(home_dir_from(get), Some(expected));
    }

    #[test]
    fn home_dir_requires_both_homedrive_and_homepath() {
        let get = env_of(&[("HOMEDRIVE", "C:")]);
        assert_eq!(home_dir_from(get), None);
    }

    #[test]
    fn home_dir_skips_empty_values() {
        let get = env_of(&[("HOME", ""), ("HOMEDRIVE", "C:"), ("HOMEPATH", "")]);
        assert_eq!(home_dir_from(get), None);
    }
}
