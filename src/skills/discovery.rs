use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use super::parser::{ParsedSkill, parse_skill_file};

const ROOTS: [&str; 3] = [".agents/skills", ".claude/skills", ".opencode/skills"];

#[derive(Debug, Clone)]
pub(crate) struct Skill {
    pub(crate) skill_dir: PathBuf,
    pub(crate) skill_file: PathBuf,
    pub(crate) description: String,
}

#[derive(Debug, Default)]
pub(crate) struct SkillRegistry {
    skills: BTreeMap<String, Skill>,
}

impl SkillRegistry {
    pub(crate) fn discover(workspace: impl AsRef<Path>) -> anyhow::Result<Self> {
        let workspace = fs::canonicalize(workspace)?;
        if !workspace.is_dir() {
            anyhow::bail!("workspace is not a directory")
        }
        let mut candidates = Vec::new();
        for (precedence, root) in ROOTS.into_iter().enumerate() {
            let root = workspace.join(root);
            let Ok(entries) = fs::read_dir(&root) else {
                continue;
            };
            let dirs = entries
                .filter_map(Result::ok)
                .filter_map(|entry| fs::canonicalize(entry.path()).ok())
                .collect::<Vec<_>>();
            for dir in dirs {
                if !dir.starts_with(&workspace) || !dir.is_dir() {
                    continue;
                }
                let file = dir.join("SKILL.md");
                let Ok(canonical_file) = fs::canonicalize(&file) else {
                    continue;
                };
                if !canonical_file.starts_with(&dir) || !canonical_file.is_file() {
                    continue;
                }
                match parse_skill_file(&canonical_file) {
                    Ok(parsed) => candidates.push((precedence, dir, canonical_file, parsed)),
                    Err(error) => {
                        tracing::warn!(path = %file.display(), %error, "ignoring malformed skill")
                    }
                }
            }
        }
        candidates.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let mut skills = BTreeMap::new();
        let mut identities = HashSet::new();
        for (
            _,
            skill_dir,
            skill_file,
            ParsedSkill {
                name, description, ..
            },
        ) in candidates
        {
            if !identities.insert(skill_dir.clone()) {
                continue;
            }
            if skills.contains_key(&name) {
                tracing::warn!(skill = %name, path = %skill_dir.display(), "ignoring skill with colliding name");
                continue;
            }
            skills.insert(
                name,
                Skill {
                    skill_dir,
                    skill_file,
                    description,
                },
            );
        }
        Ok(Self { skills })
    }

    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.skills.keys().map(String::as_str)
    }
    pub(crate) fn descriptions(&self) -> impl Iterator<Item = (&str, &str)> {
        self.skills
            .iter()
            .map(|(n, s)| (n.as_str(), s.description.as_str()))
    }
    pub(crate) fn catalog(&self) -> String {
        self.descriptions()
            .map(|(n, d)| format!("- {n}: {d}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
    pub(crate) fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }
    pub(crate) fn load(&self, name: &str) -> anyhow::Result<ParsedSkill> {
        let skill = self
            .skills
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("unknown skill: {name}"))?;
        let file = fs::canonicalize(&skill.skill_file)?;
        if !file.starts_with(&skill.skill_dir) || file != skill.skill_file {
            anyhow::bail!("registered SKILL.md changed location")
        }
        let parsed = parse_skill_file(&file)?;
        if parsed.name != name {
            anyhow::bail!("registered SKILL.md name changed")
        }
        Ok(parsed)
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    fn write(root: &Path, source: &str, dir: &str, name: &str) {
        let path = root.join(source).join(dir);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {source}\n---\nbody"),
        )
        .unwrap();
    }
    #[test]
    fn precedence_and_catalog_are_deterministic() {
        let temp = tempdir().unwrap();
        write(temp.path(), ".opencode/skills", "z", "same");
        write(temp.path(), ".agents/skills", "a", "same");
        write(temp.path(), ".claude/skills", "b", "other");
        let registry = SkillRegistry::discover(temp.path()).unwrap();
        assert_eq!(
            registry.catalog(),
            "- other: .claude/skills\n- same: .agents/skills"
        );
    }

    #[test]
    fn discovers_all_three_roots() {
        let temp = tempdir().unwrap();
        write(temp.path(), ".agents/skills", "agent", "agent");
        write(temp.path(), ".claude/skills", "claude", "claude");
        write(temp.path(), ".opencode/skills", "open", "open");
        assert_eq!(
            SkillRegistry::discover(temp.path())
                .unwrap()
                .names()
                .collect::<Vec<_>>(),
            ["agent", "claude", "open"]
        );
    }
    #[test]
    fn malformed_candidate_does_not_block_valid_one() {
        let temp = tempdir().unwrap();
        write(temp.path(), ".agents/skills", "good", "good");
        let bad = temp.path().join(".claude/skills/bad");
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join("SKILL.md"), "bad").unwrap();
        assert_eq!(
            SkillRegistry::discover(temp.path())
                .unwrap()
                .names()
                .collect::<Vec<_>>(),
            ["good"]
        );
    }

    #[test]
    fn lexical_path_breaks_same_root_name_ties() {
        let temp = tempdir().unwrap();
        write(temp.path(), ".agents/skills", "z", "same");
        write(temp.path(), ".agents/skills", "a", "same");
        assert_eq!(
            SkillRegistry::discover(temp.path())
                .unwrap()
                .get("same")
                .unwrap()
                .description,
            ".agents/skills"
        );
        assert!(
            SkillRegistry::discover(temp.path())
                .unwrap()
                .get("same")
                .unwrap()
                .skill_dir
                .ends_with("a")
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonical_identity_is_deduplicated_before_name_deduplication() {
        use std::os::unix::fs::symlink;
        let temp = tempdir().unwrap();
        write(temp.path(), ".agents/skills", "real", "real");
        let root = temp.path().join(".agents/skills");
        symlink(root.join("real"), root.join("alias")).unwrap();
        let registry = SkillRegistry::discover(temp.path()).unwrap();
        assert_eq!(registry.names().collect::<Vec<_>>(), ["real"]);
        assert!(registry.get("real").unwrap().skill_dir.ends_with("real"));
    }
}
