use crate::discovery::discover_instruction_files;
use crate::error::SkillResult;
use crate::model::{SkillDocument, SkillIndex};
use crate::parser::parse_instruction_markdown;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

pub fn load_skill_index(root: impl AsRef<Path>) -> SkillResult<SkillIndex> {
    let mut skills = Vec::new();
    for path in discover_instruction_files(root)? {
        let content = fs::read_to_string(&path)?;
        let parsed = parse_instruction_markdown(&path, &content)?;

        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let hash = format!("{:x}", hasher.finalize());

        let last_modified = fs::metadata(&path)
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|ts| ts.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);

        let id = format!(
            "{}-{}",
            normalize_id(&parsed.name),
            &hash.chars().take(12).collect::<String>()
        );

        skills.push(SkillDocument {
            id,
            name: parsed.name,
            description: parsed.description,
            version: parsed.version,
            license: parsed.license,
            compatibility: parsed.compatibility,
            tags: parsed.tags,
            allowed_tools: parsed.allowed_tools,
            references: parsed.references,
            trigger: parsed.trigger,
            hint: parsed.hint,
            metadata: parsed.metadata,
            body: parsed.body,
            path,
            hash,
            last_modified,
        });
    }

    skills.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    Ok(SkillIndex::new(skills))
}

fn normalize_id(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if c == ' ' || c == '-' || c == '_' {
            out.push('-');
        }
    }
    if out.is_empty() { "skill".to_string() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn loads_index_with_hash_and_summary_fields() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".skills")).unwrap();
        fs::write(
            root.join(".skills/search.md"),
            "---\nname: search\ndescription: Search docs\n---\nUse rg first.",
        )
        .unwrap();

        let index = load_skill_index(root).unwrap();
        assert_eq!(index.len(), 1);
        let skill = &index.skills()[0];
        assert_eq!(skill.name, "search");
        assert!(!skill.hash.is_empty());
        assert!(skill.last_modified.is_some());
    }

    #[test]
    fn loads_agents_md_as_skill_document() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("AGENTS.md"), "# Repo Instructions\nUse cargo test before commit.\n")
            .unwrap();

        let index = load_skill_index(root).unwrap();
        assert_eq!(index.len(), 1);
        let skill = &index.skills()[0];
        assert_eq!(skill.name, "agents");
        assert!(skill.tags.iter().any(|t| t == "agents-md"));
        assert!(skill.body.contains("Use cargo test before commit."));
    }

    #[test]
    fn loads_root_soul_md_as_skill_document() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(root.join("SOUL.MD"), "# Soul\nBias toward deterministic workflows.\n").unwrap();
        fs::create_dir_all(root.join("pkg")).unwrap();
        fs::write(root.join("pkg/SOUL.md"), "# Nested soul should not load\n").unwrap();

        let index = load_skill_index(root).unwrap();
        assert_eq!(index.len(), 1);
        let skill = &index.skills()[0];
        assert_eq!(skill.name, "soul");
        assert!(skill.tags.iter().any(|t| t == "soul-md"));
        assert!(skill.body.contains("deterministic workflows"));
    }
}
