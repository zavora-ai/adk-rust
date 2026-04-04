use crate::error::{SkillError, SkillResult};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const CONVENTION_FILES: &[&str] =
    &["AGENTS.md", "AGENT.md", "CLAUDE.md", "GEMINI.md", "COPILOT.md", "SKILLS.md", "SOUL.md"];

const IGNORED_DIRS: &[&str] =
    &[".git", ".hg", ".svn", "target", "node_modules", ".next", "dist", "build", "coverage"];

/// Discovers all Markdown skill files under the `.skills` and `.claude/skills/`
/// directories at `root`.
///
/// Recursively walks both directories, collecting `.md` files and excluding
/// known support subdirectories (e.g. `references/`, `agents/`, `scripts/`).
/// Returns an empty list if neither directory exists. Non-existent or
/// non-directory paths are silently skipped.
pub fn discover_skill_files(root: impl AsRef<Path>) -> SkillResult<Vec<PathBuf>> {
    let root = root.as_ref();
    let dirs = vec![root.join(".skills"), root.join(".claude").join("skills")];
    discover_skill_files_from_dirs(&dirs)
}

/// Walks each directory in `dirs` (if it exists and is a directory), collects
/// `.md` files excluding support dirs, and returns a merged, deduplicated,
/// sorted list.
fn discover_skill_files_from_dirs(dirs: &[PathBuf]) -> SkillResult<Vec<PathBuf>> {
    let mut files = Vec::new();

    for dir in dirs {
        if !dir.exists() || !dir.is_dir() {
            continue;
        }

        let dir_files = WalkDir::new(dir)
            .into_iter()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "md"))
            .filter(|entry| !is_skill_support_file(entry.path()))
            .map(|entry| entry.into_path());

        files.extend(dir_files);
    }

    files.sort();
    files.dedup();
    Ok(files)
}

/// Returns true for files inside known supporting subdirectories of a skill
/// (e.g. `references/`, `agents/`, `scripts/`). These are resources referenced
/// by a `SKILL.md`, not skill definitions themselves.
fn is_skill_support_file(path: &Path) -> bool {
    const SUPPORT_DIRS: &[&str] = &["references", "agents", "scripts"];
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        SUPPORT_DIRS.iter().any(|d| s.eq_ignore_ascii_case(d))
    })
}

/// Discovers all Markdown skill files under the `.skills/` and `.claude/skills/`
/// directories at `root`, plus any additional directories in `extra_dirs`.
///
/// Merges project-local directories with the provided extra directories.
/// Non-existent or non-directory extra paths are silently skipped.
/// Returns a sorted, deduplicated list of discovered `.md` files.
pub fn discover_skill_files_with_extras(
    root: impl AsRef<Path>,
    extra_dirs: &[PathBuf],
) -> SkillResult<Vec<PathBuf>> {
    let root = root.as_ref();
    let mut dirs = vec![root.join(".skills"), root.join(".claude").join("skills")];
    dirs.extend(extra_dirs.iter().cloned());
    discover_skill_files_from_dirs(&dirs)
}

/// Discovers all instruction files: both `.skills/` Markdown files and
/// convention files (e.g. `AGENTS.md`, `CLAUDE.md`, `SOUL.md`) found anywhere
/// in the project tree, excluding common build and dependency directories.
/// Results are sorted and deduplicated.
pub fn discover_instruction_files(root: impl AsRef<Path>) -> SkillResult<Vec<PathBuf>> {
    let root = root.as_ref();
    let mut files = discover_skill_files(root)?;
    files.extend(discover_convention_files(root)?);
    files.sort();
    files.dedup();
    Ok(files)
}

/// Discovers all instruction files from project-local directories, extra
/// directories, and convention files. Merges skill files from `.skills/`,
/// `.claude/skills/`, and the provided `extra_dirs` with convention files
/// found in the project tree. Non-existent or non-directory extra paths are
/// silently skipped. Results are sorted and deduplicated.
pub fn discover_instruction_files_with_extras(
    root: impl AsRef<Path>,
    extra_dirs: &[PathBuf],
) -> SkillResult<Vec<PathBuf>> {
    let root = root.as_ref();
    let mut files = discover_skill_files_with_extras(root, extra_dirs)?;
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
    fn discovers_skill_files_from_claude_skills_dir() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".claude/skills")).unwrap();

        fs::write(root.join(".claude/skills/c.md"), "---\nname: c\ndescription: c\n---\n").unwrap();

        let files = discover_skill_files(root).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("c.md"));
    }

    #[test]
    fn discovers_skill_files_from_both_dirs() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".skills")).unwrap();
        fs::create_dir_all(root.join(".claude/skills")).unwrap();

        fs::write(root.join(".skills/a.md"), "---\nname: a\ndescription: a\n---\n").unwrap();
        fs::write(root.join(".claude/skills/b.md"), "---\nname: b\ndescription: b\n---\n").unwrap();

        let files = discover_skill_files(root).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|p| p.ends_with("a.md")));
        assert!(files.iter().any(|p| p.ends_with("b.md")));
        // Verify sorted
        assert!(files[0] < files[1]);
    }

    #[test]
    fn discovers_skill_files_returns_empty_when_neither_dir_exists() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        let files = discover_skill_files(root).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn discovers_skill_files_skips_non_directory_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        // Create .skills as a file, not a directory
        fs::write(root.join(".skills"), "not a directory").unwrap();

        let files = discover_skill_files(root).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn discover_skill_files_from_dirs_deduplicates() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("shared");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("x.md"), "---\nname: x\n---\n").unwrap();

        // Pass the same directory twice
        let files = discover_skill_files_from_dirs(&[dir.clone(), dir]).unwrap();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn discover_instruction_files_includes_claude_skills() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".claude/skills")).unwrap();

        fs::write(
            root.join(".claude/skills/skill.md"),
            "---\nname: skill\ndescription: skill\n---\n",
        )
        .unwrap();
        fs::write(root.join("AGENTS.md"), "# instructions\n").unwrap();

        let files = discover_instruction_files(root).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|p| p.ends_with("skill.md")));
        assert!(files.iter().any(|p| p.ends_with("AGENTS.md")));
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
