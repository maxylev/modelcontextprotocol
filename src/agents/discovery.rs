use crate::agents::{
    definition::AgentDefinition,
    markdown::{MarkdownFlavor, parse_markdown},
    toml::parse_toml,
};
use anyhow::{Context, Result, bail};
use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Clone)]
pub(crate) struct AgentRegistry {
    workspace: PathBuf,
    agents: BTreeMap<String, Arc<AgentDefinition>>,
}
impl AgentRegistry {
    pub(crate) fn discover(workspace: impl AsRef<Path>) -> Result<Self> {
        let workspace = fs::canonicalize(workspace.as_ref()).context("canonicalizing workspace")?;
        if !workspace.is_dir() {
            bail!("workspace must be a directory")
        }
        let specs = [
            (".agents/agents", Some(MarkdownFlavor::Canonical), true),
            (".claude/agents", Some(MarkdownFlavor::Claude), false),
            (".codex/agents", None, false),
            (".opencode/agents", Some(MarkdownFlavor::OpenCode), false),
        ];
        let mut candidates = Vec::new();
        for (precedence, (relative, markdown, canonical_toml)) in specs.into_iter().enumerate() {
            collect(
                &workspace.join(relative),
                &workspace,
                markdown,
                canonical_toml,
                precedence as u8,
                &mut candidates,
            );
        }
        candidates.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let mut paths = BTreeSet::new();
        let mut agents = BTreeMap::new();
        let mut warned = BTreeSet::new();
        for (_, path, flavor) in candidates {
            if !paths.insert(path.clone()) {
                continue;
            }
            let input = match fs::read_to_string(&path) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(path=%path.display(),error=%e,"cannot read agent");
                    continue;
                }
            };
            let parsed = match flavor {
                Some(f) => parse_markdown(path.clone(), &input, f),
                None => parse_toml(path.clone(), &input).map(Some),
            };
            match parsed {
                Ok(Some(agent)) => {
                    let name = agent.name.clone();
                    match agents.entry(name) {
                        Entry::Occupied(entry) => {
                            if warned.insert(entry.key().clone()) {
                                tracing::warn!(name=%entry.key(),"agent name collision; retaining higher-precedence definition")
                            }
                        }
                        Entry::Vacant(entry) => {
                            entry.insert(Arc::new(agent));
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(path=%path.display(),error=%e,"ignoring malformed agent"),
            }
        }
        Ok(Self { workspace, agents })
    }
    pub(crate) fn names(&self) -> Vec<String> {
        self.agents.keys().cloned().collect()
    }
    pub(crate) fn catalog(&self) -> Vec<Arc<AgentDefinition>> {
        self.agents.values().cloned().collect()
    }
    pub(crate) fn get(&self, name: &str) -> Option<Arc<AgentDefinition>> {
        self.agents.get(name).cloned()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }
    pub(crate) fn workspace(&self) -> &Path {
        &self.workspace
    }
}
fn collect(
    root: &Path,
    workspace: &Path,
    markdown: Option<MarkdownFlavor>,
    canonical_toml: bool,
    precedence: u8,
    out: &mut Vec<(u8, PathBuf, Option<MarkdownFlavor>)>,
) {
    let Ok(root) = fs::canonicalize(root) else {
        return;
    };
    if !root.is_dir() {
        return;
    }
    if !root.starts_with(workspace) {
        tracing::warn!(path=%root.display(),"agent root escapes workspace");
        return;
    }
    let mut visited = BTreeSet::new();
    let mut scan = ScanContext {
        root: &root,
        workspace,
        markdown,
        canonical_toml,
        precedence,
        visited: &mut visited,
        out,
    };
    scan.collect_dir(&root, 0);
}
struct ScanContext<'a> {
    root: &'a Path,
    workspace: &'a Path,
    markdown: Option<MarkdownFlavor>,
    canonical_toml: bool,
    precedence: u8,
    visited: &'a mut BTreeSet<PathBuf>,
    out: &'a mut Vec<(u8, PathBuf, Option<MarkdownFlavor>)>,
}

impl ScanContext<'_> {
    fn collect_dir(&mut self, dir: &Path, depth: usize) {
        let Ok(dir) = fs::canonicalize(dir) else {
            return;
        };
        if !dir.starts_with(self.workspace)
            || !dir.starts_with(self.root)
            || !self.visited.insert(dir.clone())
        {
            return;
        };
        let Ok(entries) = fs::read_dir(&dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for entry in entries {
            let Ok(target) = fs::canonicalize(&entry) else {
                continue;
            };
            if !target.starts_with(self.workspace) || !target.starts_with(self.root) {
                tracing::warn!(path=%entry.display(),"agent path escapes root");
                continue;
            }
            let Ok(meta) = fs::metadata(&entry) else {
                continue;
            };
            if meta.is_dir() {
                if depth < 8 {
                    self.collect_dir(&entry, depth + 1)
                }
            } else if meta.is_file() {
                let ext = target.extension().and_then(|v| v.to_str());
                match (ext, self.markdown) {
                    (Some("toml"), None) | (Some("toml"), Some(_)) if self.canonical_toml => {
                        self.out.push((self.precedence, target, None))
                    }
                    (Some("md"), Some(flavor)) => {
                        self.out.push((self.precedence, target, Some(flavor)))
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn write(root: &Path, name: &str, body: &str) {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::File::create(path)
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
    }
    #[test]
    fn same_precedence_uses_lexical_canonical_path_and_skips_malformed() {
        let temp = tempfile::tempdir().unwrap();
        let canonical = "---\nname: same\ndescription: x\nmodel: g\nmodelProvider: openai\n---\na";
        write(temp.path(), ".agents/agents/a.md", canonical);
        write(temp.path(), ".agents/agents/z.md", canonical);
        write(
            temp.path(),
            ".claude/agents/bad.md",
            "---\nname: bad\ndescription: x\nmodel: c\nisolation: vm\n---\nx",
        );
        let registry = AgentRegistry::discover(temp.path()).unwrap();
        assert!(registry.get("bad").is_none());
        assert!(registry.get("same").unwrap().source_path.ends_with("a.md"));
    }
    #[cfg(unix)]
    #[test]
    fn file_symlink_alias_is_deduplicated() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let body = "---\nname: a\ndescription: x\nmodel: g\nmodelProvider: openai\n---\nx";
        write(temp.path(), ".agents/agents/real.md", body);
        symlink(
            temp.path().join(".agents/agents/real.md"),
            temp.path().join(".agents/agents/alias.md"),
        )
        .unwrap();
        assert_eq!(
            AgentRegistry::discover(temp.path()).unwrap().names(),
            vec!["a"]
        );
    }
}
