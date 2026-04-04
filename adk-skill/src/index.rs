use crate::discovery::{discover_instruction_files, discover_instruction_files_with_extras};
use crate::error::SkillResult;
use crate::model::{SkillDocument, SkillIndex};
use crate::parser::parse_instruction_markdown;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Loads a [`SkillIndex`] by discovering and parsing all instruction files under `root`.
///
/// Each file is read, parsed, and assigned a content-hash-based identifier.
/// The resulting index is sorted by skill name and path.
pub fn load_skill_index(root: impl AsRef<Path>) -> SkillResult<SkillIndex> {
    let mut skills = Vec::new();
    for path in discover_instruction_files(root)? {
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        // Skip files that don't have valid skill/instruction format.
        // This allows non-skill .md files (reference docs, READMEs, etc.)
        // to coexist under .skills/ without causing parse errors.
        let parsed = match parse_instruction_markdown(&path, &content) {
            Ok(p) => p,
            Err(_) => continue,
        };

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
            triggers: parsed.triggers,
        });
    }

    skills.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    Ok(SkillIndex::new(skills))
}

/// Loads a [`SkillIndex`] by discovering and parsing all instruction files under `root`,
/// plus any additional directories in `extra_dirs`.
///
/// Merges project-local instruction files with files from the provided extra directories.
/// Non-existent or non-directory extra paths are silently skipped.
/// Each file is read, parsed, and assigned a content-hash-based identifier.
/// The resulting index is sorted by skill name and path, then deduplicated by name.
/// Project-local skills (`.skills/`, `.claude/skills/`) take precedence over global/extra
/// paths because discovery lists project-local directories first, and deduplication
/// keeps the first occurrence.
pub fn load_skill_index_with_extras(
    root: impl AsRef<Path>,
    extra_dirs: &[PathBuf],
) -> SkillResult<SkillIndex> {
    let root = root.as_ref();
    let mut skills = Vec::new();
    for path in discover_instruction_files_with_extras(root, extra_dirs)? {
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let parsed = match parse_instruction_markdown(&path, &content) {
            Ok(p) => p,
            Err(_) => continue,
        };

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
            triggers: parsed.triggers,
        });
    }

    // Deduplicate by name, preferring project-local skills (.skills/, .claude/skills/)
    // over global/extra paths. We build a map keyed by name; project-local entries
    // always win over non-local entries, and among entries of the same locality the
    // first one encountered (lowest path order) wins.
    let local_prefixes = [root.join(".skills"), root.join(".claude").join("skills")];
    let is_project_local =
        |path: &Path| local_prefixes.iter().any(|prefix| path.starts_with(prefix));

    let mut by_name: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut deduped: Vec<SkillDocument> = Vec::with_capacity(skills.len());

    for skill in skills {
        match by_name.get(&skill.name) {
            Some(&idx) => {
                // Replace only if the new skill is project-local and the existing one is not
                if is_project_local(&skill.path) && !is_project_local(&deduped[idx].path) {
                    deduped[idx] = skill;
                }
                // Otherwise keep the existing entry (first wins within same locality)
            }
            None => {
                by_name.insert(skill.name.clone(), deduped.len());
                deduped.push(skill);
            }
        }
    }

    deduped.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    Ok(SkillIndex::new(deduped))
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
    fn skips_non_skill_md_files_in_subdirectories() {
        // Reproduces issue #204: reference docs without frontmatter
        // should be silently skipped, not cause InvalidFrontmatter errors
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".skills/my-skill/references")).unwrap();
        fs::create_dir_all(root.join(".skills/my-skill/assets")).unwrap();

        // Valid skill
        fs::write(
            root.join(".skills/my-skill/skill.md"),
            "---\nname: my-skill\ndescription: A skill\n---\nBody",
        )
        .unwrap();

        // Non-skill .md files (no frontmatter) — must not cause errors
        fs::write(
            root.join(".skills/my-skill/references/docs.md"),
            "# Reference Documentation\nThis is supporting docs.",
        )
        .unwrap();
        fs::write(root.join(".skills/my-skill/assets/notes.md"), "Just plain text notes.").unwrap();

        // Also a random .md at skill level without frontmatter
        fs::write(
            root.join(".skills/my-skill/README.md"),
            "# My Skill README\nNo frontmatter here.",
        )
        .unwrap();

        let index = load_skill_index(root).unwrap();
        // Only the valid skill.md should be indexed
        assert_eq!(index.len(), 1);
        assert_eq!(index.skills()[0].name, "my-skill");
    }

    #[test]
    fn load_with_extras_deduplicates_by_name_preferring_project_local() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let extra = tempfile::tempdir().unwrap();

        // Project-local skill in .skills/
        fs::create_dir_all(root.join(".skills")).unwrap();
        fs::write(
            root.join(".skills/search.md"),
            "---\nname: search\ndescription: Local search\n---\nLocal body.",
        )
        .unwrap();

        // Same-named skill in extra dir (global)
        fs::write(
            extra.path().join("search.md"),
            "---\nname: search\ndescription: Global search\n---\nGlobal body.",
        )
        .unwrap();

        let index = load_skill_index_with_extras(root, &[extra.path().to_path_buf()]).unwrap();

        // Only one "search" skill should remain
        let search_skills: Vec<_> = index.skills().iter().filter(|s| s.name == "search").collect();
        assert_eq!(search_skills.len(), 1);
        // The project-local version wins
        assert_eq!(search_skills[0].description, "Local search");
        assert!(search_skills[0].path.starts_with(root));
    }

    #[test]
    fn load_with_extras_keeps_distinct_names() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let extra = tempfile::tempdir().unwrap();

        fs::create_dir_all(root.join(".skills")).unwrap();
        fs::write(
            root.join(".skills/alpha.md"),
            "---\nname: alpha\ndescription: Alpha\n---\nAlpha body.",
        )
        .unwrap();

        fs::write(
            extra.path().join("beta.md"),
            "---\nname: beta\ndescription: Beta\n---\nBeta body.",
        )
        .unwrap();

        let index = load_skill_index_with_extras(root, &[extra.path().to_path_buf()]).unwrap();

        assert_eq!(index.len(), 2);
        assert!(index.find_by_name("alpha").is_some());
        assert!(index.find_by_name("beta").is_some());
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
