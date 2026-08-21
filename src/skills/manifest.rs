use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

pub(crate) const MAX_RESOURCES: usize = 1000;
pub(crate) const MAX_DEPTH: usize = 8;

pub(crate) fn resource_manifest(skill_dir: &Path) -> anyhow::Result<Vec<String>> {
    let root = fs::canonicalize(skill_dir)?;
    let mut resources = Vec::new();
    let mut identities = HashSet::new();
    collect(&root, &root, 0, &mut resources, &mut identities)?;
    resources.sort();
    Ok(resources)
}

fn collect(
    root: &Path,
    dir: &Path,
    depth: usize,
    out: &mut Vec<String>,
    identities: &mut HashSet<PathBuf>,
) -> anyhow::Result<()> {
    let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| anyhow::anyhow!("resource is outside skill directory"))?;
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_dir() {
            if depth >= MAX_DEPTH {
                anyhow::bail!("resource directory depth exceeds {MAX_DEPTH}")
            }
            collect(root, &path, depth + 1, out, identities)?;
        } else {
            if relative == Path::new("SKILL.md") {
                continue;
            }
            let target = fs::canonicalize(&path)?;
            if !target.starts_with(root) {
                anyhow::bail!(
                    "resource target escapes skill directory: {}",
                    relative.display()
                )
            }
            if !fs::metadata(&target)?.is_file() {
                continue;
            }
            if depth + 1 > MAX_DEPTH {
                anyhow::bail!("resource depth exceeds {MAX_DEPTH}")
            }
            if !identities.insert(target) {
                continue;
            }
            if out.len() >= MAX_RESOURCES {
                anyhow::bail!("resource count exceeds {MAX_RESOURCES}")
            }
            out.push(slash_path(relative));
        }
    }
    Ok(())
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn lists_sorted_resources_without_skill() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("SKILL.md"), "x").unwrap();
        fs::create_dir(temp.path().join("nested")).unwrap();
        fs::write(temp.path().join("nested/z.txt"), "z").unwrap();
        fs::write(temp.path().join("a.txt"), "a").unwrap();
        assert_eq!(
            resource_manifest(temp.path()).unwrap(),
            ["a.txt", "nested/z.txt"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_file_symlinks_that_escape_the_skill() {
        use std::os::unix::fs::symlink;
        let skill = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret"), "x").unwrap();
        symlink(outside.path().join("secret"), skill.path().join("escape")).unwrap();
        assert!(resource_manifest(skill.path()).is_err());
    }

    #[test]
    fn enforces_resource_count_and_depth() {
        let temp = tempdir().unwrap();
        for index in 0..=MAX_RESOURCES {
            fs::write(temp.path().join(format!("{index}.txt")), "x").unwrap();
        }
        assert!(resource_manifest(temp.path()).is_err());

        let deep = tempdir().unwrap();
        let mut dir = deep.path().to_path_buf();
        for index in 0..MAX_DEPTH {
            dir.push(format!("d{index}"));
            fs::create_dir(&dir).unwrap();
        }
        fs::write(dir.join("too-deep"), "x").unwrap();
        assert!(resource_manifest(deep.path()).is_err());
    }
}
