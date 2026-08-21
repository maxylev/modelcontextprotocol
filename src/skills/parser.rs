use std::{fs, path::Path};

use serde::Deserialize;

pub(crate) const MAX_SKILL_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedSkill {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) instructions: String,
}

#[derive(Deserialize)]
struct Frontmatter {
    name: String,
    description: String,
}

pub(crate) fn parse_skill_file(path: &Path) -> anyhow::Result<ParsedSkill> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_SKILL_BYTES {
        anyhow::bail!("SKILL.md exceeds the 1 MiB limit")
    }
    let text = fs::read_to_string(path)?;
    parse_skill(&text)
}

pub(crate) fn parse_skill(text: &str) -> anyhow::Result<ParsedSkill> {
    let mut lines = text.split_inclusive('\n');
    let Some(first) = lines.next() else {
        anyhow::bail!("SKILL.md is empty")
    };
    if first.trim_end_matches(['\r', '\n']) != "---" {
        anyhow::bail!("SKILL.md must begin with YAML frontmatter")
    }
    let mut yaml = String::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            closed = true;
            break;
        }
        yaml.push_str(line);
    }
    if !closed {
        anyhow::bail!("SKILL.md frontmatter is not closed")
    }
    let frontmatter: Frontmatter = serde_yaml::from_str(&yaml)
        .map_err(|e| anyhow::anyhow!("invalid SKILL.md frontmatter: {e}"))?;
    if !valid_name(&frontmatter.name) {
        anyhow::bail!("skill name must match ^[a-z0-9]+(-[a-z0-9]+)*$ and be at most 64 characters")
    }
    if frontmatter.description.trim().is_empty() {
        anyhow::bail!("skill description must not be empty")
    }
    Ok(ParsedSkill {
        name: frontmatter.name,
        description: frontmatter.description,
        instructions: lines.collect(),
    })
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};
    use tempfile::NamedTempFile;

    #[test]
    fn parses_and_removes_frontmatter() {
        let skill = parse_skill("---\nname: web-search\ndescription: Search web\n---\nUse this.\n")
            .unwrap();
        assert_eq!(skill.name, "web-search");
        assert_eq!(skill.instructions, "Use this.\n");
    }

    #[test]
    fn rejects_invalid_names_and_missing_frontmatter() {
        for name in ["Upper", "two--dash", "-start", "end-", ""] {
            assert!(parse_skill(&format!("---\nname: {name:?}\ndescription: x\n---\n")).is_err());
        }
        assert!(parse_skill("name: x").is_err());
        assert!(parse_skill("---\nname: demo\n---\n").is_err());
        assert!(parse_skill("---\ndescription: Demo\n---\n").is_err());
    }

    #[test]
    fn permits_extra_frontmatter_metadata() {
        let skill = parse_skill("---\nname: demo\ndescription: Demo\nlicense: MIT\nmetadata:\n  author: agent\n---\nbody\n").unwrap();
        assert_eq!(skill.name, "demo");
    }

    #[test]
    fn rejects_oversized_files_without_reading_them() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"x").unwrap();
        file.as_file_mut()
            .seek(SeekFrom::Start(MAX_SKILL_BYTES))
            .unwrap();
        file.write_all(b"x").unwrap();
        assert!(
            parse_skill_file(file.path())
                .unwrap_err()
                .to_string()
                .contains("1 MiB")
        );
    }
}
