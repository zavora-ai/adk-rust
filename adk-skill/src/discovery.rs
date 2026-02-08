use crate::error::{SkillError, SkillResult};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const CONVENTION_FILES: &[&str] =
    &["AGENTS.md", "AGENT.md", "CLAUDE.md", "GEMINI.md", "COPILOT.md", "SKILLS.md", "SOUL.md"];

const IGNORED_DIRS: &[&str] =
    &[".git", ".hg", ".svn", "target", "node_modules", ".next", "dist", "build", "coverage"];

pub fn discover_skill_files(root: impl AsRef<Path>) -> SkillResult<Vec<PathBuf>> {
    let skill_root = root.as_ref().join(".skills");
    if !skill_root.exists() {
        return Ok(Vec::new());
    }
    if !skill_root.is_dir() {
        return Err(SkillError::InvalidSkillsRoot(skill_root));
    }

    let mut files = WalkDir::new(&skill_root)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "md"))
        .map(|entry| entry.into_path())
        .collect::<Vec<_>>();

    files.sort();
    Ok(files)
}

pub fn discover_instruction_files(root: impl AsRef<Path>) -> SkillResult<Vec<PathBuf>> {
    let root = root.as_ref();
    let mut files = discover_skill_files(root)?;
    files.extend(discover_convention_files(root)?);
    files.sort();
    files.dedup();
    Ok(files)
}

fn discover_convention_files(root: &Path) -> SkillResult<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    if !root.is_dir() {
        return Err(SkillError::InvalidSkillsRoot(root.to_path_buf()));
    }

    let mut files = WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| !is_ignored_dir(entry))
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| is_convention_file(entry.path(), root))
        .map(|entry| entry.into_path())
        .collect::<Vec<_>>();

    files.sort();
    Ok(files)
}

fn is_ignored_dir(entry: &walkdir::DirEntry) -> bool {
    entry.file_type().is_dir()
        && entry.file_name().to_str().is_some_and(|name| {
            IGNORED_DIRS.iter().any(|ignored| name.eq_ignore_ascii_case(ignored))
        })
}

fn is_convention_file(path: &Path, root: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };

    if !CONVENTION_FILES.iter().any(|candidate| name.eq_ignore_ascii_case(candidate)) {
        return false;
    }

    // SOUL.md is currently supported as a repository-root convention file.
    if name.eq_ignore_ascii_case("SOUL.md") {
        return path.parent().is_some_and(|parent| parent == root);
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discovers_only_markdown_skill_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".skills/nested")).unwrap();

        fs::write(root.join(".skills/a.md"), "---\nname: a\ndescription: a\n---\n").unwrap();
        fs::write(root.join(".skills/nested/b.md"), "---\nname: b\ndescription: b\n---\n").unwrap();
        fs::write(root.join(".skills/notes.txt"), "ignore").unwrap();

        let files = discover_skill_files(root).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|p| p.extension().is_some_and(|ext| ext == "md")));
    }

    #[test]
    fn discovers_convention_instruction_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("pkg")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();

        fs::write(root.join("AGENTS.md"), "# Root instructions\n").unwrap();
        fs::write(root.join("pkg/CLAUDE.md"), "# Claude instructions\n").unwrap();
        fs::write(root.join("SOUL.MD"), "# Soul instructions\n").unwrap();
        fs::write(root.join("pkg/SOUL.md"), "# Nested soul should be ignored\n").unwrap();
        fs::write(root.join("pkg/readme.md"), "# ignore\n").unwrap();
        fs::write(root.join("target/GEMINI.md"), "# ignored by target skip\n").unwrap();

        let files = discover_instruction_files(root).unwrap();
        assert_eq!(files.len(), 3);
        assert!(files.iter().any(|p| p.ends_with("AGENTS.md")));
        assert!(files.iter().any(|p| p.ends_with("CLAUDE.md")));
        assert!(files.iter().any(|p| p.ends_with("SOUL.MD")));
        assert!(!files.iter().any(|p| p.ends_with("pkg/SOUL.md")));
    }
}
