//! Protected paths a read-write sandbox keeps read-only (spec 3.1 §9.2 I26, I29; §10 S3).

use std::path::{Path, PathBuf};

use crate::approvals::matcher::is_protected_path;

/// Paths at the workspace root that a read-write sandbox keeps read-only
/// (spec 3.1 §9.1 "Shell", §9.2 I26).
///
/// They are the protected names of spec 3.1 §9.1 as they appear at the root, with `.cargo`
/// standing for `.cargo/config` and `.cargo/config.toml`. A sandboxed command cannot write,
/// create, remove or rename anything beneath one that exists (adk-sandbox S3). One that does
/// not exist cannot be protected from creation, so `auto_allow_sandboxed` turns on only when
/// all of them exist.
///
/// A `.git` directory stands for its `hooks`, `config` and `info` when all three exist and
/// none is a symlink: they decide which commands git runs, and the index, refs and objects
/// stay writable, so `git add` and `git commit` work inside the sandbox (spec 3.1 §7.33
/// fix 7). Otherwise the whole directory is kept read-only. In a linked worktree or a
/// submodule checkout `.git` is a file naming the gitdir, and the file is kept read-only.
///
/// # Example
///
/// ```rust
/// assert!(adk_harness::SANDBOX_PROTECTED_PATHS.contains(&".git"));
/// assert!(adk_harness::SANDBOX_PROTECTED_PATHS.contains(&"AGENTS.md"));
/// ```
pub const SANDBOX_PROTECTED_PATHS: &[&str] = &[
    ".git",
    ".adk",
    ".skills",
    ".claude",
    ".cargo",
    "AGENTS.md",
    "AGENT.md",
    "CLAUDE.md",
    "MEMORY.md",
    ".envrc",
    "lefthook.yml",
    "lefthook.yaml",
];

/// How deep below the root nested protected paths are looked for; the depth of the
/// instruction index (spec 3.1 §7.7).
const NESTED_DEPTH: usize = 4;
/// The entries of a gitdir a read-write sandbox keeps read-only in place of the whole
/// gitdir.
const GITDIR_PROTECTED: &[&str] = &["hooks", "config", "info"];
/// How many levels below a gitdir's `modules` submodule gitdirs are looked for; a
/// submodule's name may contain slashes.
const MODULES_DEPTH: usize = 8;
/// At most this many directory entries are visited looking for nested protected paths.
const NESTED_VISIT_LIMIT: usize = 20_000;
/// Build output and dependency trees, never searched for nested protected paths.
const NESTED_SKIP: &[&str] = &["target", "node_modules"];

/// The protected paths of one workspace, as the sandbox can and cannot keep them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ProtectedPaths {
    /// Existing protected paths to grant read-only, inside the root once resolved.
    pub(crate) present: Vec<PathBuf>,
    /// Root-level protected paths the sandbox cannot keep read-only, with the reason.
    pub(crate) unprotectable: Vec<(String, &'static str)>,
}

impl ProtectedPaths {
    /// The unprotectable paths as `name (reason)`, for a refusal message.
    #[cfg(feature = "sandbox")]
    pub(crate) fn describe_unprotectable(&self) -> String {
        self.unprotectable
            .iter()
            .map(|(name, reason)| format!("{name} ({reason})"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Finds the protected paths of the workspace at `root` (canonical).
///
/// Root-level names come from [`SANDBOX_PROTECTED_PATHS`]. Below the root, existing
/// protected paths are found up to four levels deep, skipping `target` and `node_modules`.
/// A `.git` directory, at the root or below it, contributes what [`push_gitdir`] finds.
/// A symlinked protected path protects its target only when the target is inside the
/// root, and is reported unprotectable, since the link itself can be replaced.
pub(crate) fn protected_paths(root: &Path) -> ProtectedPaths {
    let mut found = ProtectedPaths::default();
    for name in SANDBOX_PROTECTED_PATHS {
        let path = root.join(name);
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            found.unprotectable.push(((*name).to_string(), "does not exist"));
            continue;
        };
        if metadata.file_type().is_symlink() {
            found.unprotectable.push(((*name).to_string(), "is a symlink"));
        }
        // A read-only grant of a target outside the root would let the sandbox read it.
        if !std::fs::canonicalize(&path).is_ok_and(|resolved| resolved.starts_with(root)) {
            continue;
        }
        if *name == ".git" && metadata.is_dir() {
            push_gitdir(&path, &mut found.present);
        } else {
            found.present.push(path);
        }
    }
    let mut visited = 0;
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            visited += 1;
            if visited > NESTED_VISIT_LIMIT {
                return found;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else { continue };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if depth == 0 && SANDBOX_PROTECTED_PATHS.contains(&name.as_ref()) {
                continue;
            }
            let Ok(relative) = path.strip_prefix(root) else { continue };
            if file_type.is_dir() && name.eq_ignore_ascii_case(".git") {
                push_gitdir(&path, &mut found.present);
            } else if !file_type.is_symlink() && is_protected_path(&relative.to_string_lossy()) {
                found.present.push(path);
            } else if file_type.is_dir()
                && depth + 1 < NESTED_DEPTH
                && !NESTED_SKIP.contains(&name.as_ref())
            {
                pending.push((path, depth + 1));
            }
        }
    }
    found
}

/// Adds what a read-write sandbox keeps read-only of the gitdir at `gitdir`: its `hooks`,
/// `config` and `info` when all three exist and none is a symlink, and then the same for
/// each submodule gitdir under its `modules`; otherwise the whole gitdir. A directory holding
/// a `HEAD` file is a gitdir; symlinks are not followed.
fn push_gitdir(gitdir: &Path, present: &mut Vec<PathBuf>) {
    let mut gitdirs = vec![(gitdir.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = gitdirs.pop() {
        let entries: Vec<PathBuf> = GITDIR_PROTECTED.iter().map(|name| dir.join(name)).collect();
        if !entries.iter().all(|entry| {
            std::fs::symlink_metadata(entry).is_ok_and(|metadata| !metadata.is_symlink())
        }) {
            present.push(dir);
            continue;
        }
        present.extend(entries);
        let mut pending = vec![(dir.join("modules"), depth)];
        while let Some((parent, depth)) = pending.pop() {
            if depth >= MODULES_DEPTH {
                continue;
            }
            let Ok(children) = std::fs::read_dir(&parent) else { continue };
            for child in children.flatten() {
                if !child.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                let path = child.path();
                if path.join("HEAD").is_file() {
                    gitdirs.push((path, depth + 1));
                } else {
                    pending.push((path, depth + 1));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The symlink cases are Unix-only: creating a symlink on Windows needs a privilege the
    /// test runner does not hold.
    #[test]
    fn missing_and_symlinked_root_paths_are_unprotectable_and_nested_ones_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        for sub in [".git/hooks", "crates/a/.git/hooks", "crates/a/.cargo", "target/x/.git"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        for file in ["AGENTS.md", "crates/a/AGENTS.md", "crates/a/.cargo/config.toml"] {
            std::fs::write(root.join(file), "x").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink("AGENTS.md", root.join("CLAUDE.md")).unwrap();

        let found = protected_paths(&root);

        let mut present: Vec<PathBuf> = found
            .present
            .iter()
            .map(|path| path.strip_prefix(&root).unwrap().to_path_buf())
            .collect();
        present.sort();
        let mut expected: Vec<PathBuf> = [
            // Neither gitdir has all of hooks, config and info, so both stay read-only whole.
            ".git",
            "AGENTS.md",
            "crates/a/.cargo/config.toml",
            "crates/a/.git",
            "crates/a/AGENTS.md",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        if cfg!(unix) {
            expected.push(PathBuf::from("CLAUDE.md"));
        }
        expected.sort();
        assert_eq!(present, expected);
        let unprotectable: Vec<&str> =
            found.unprotectable.iter().map(|(name, _)| name.as_str()).collect();
        assert!(unprotectable.contains(&".envrc"));
        assert!(!unprotectable.contains(&".git"));
        if cfg!(unix) {
            assert!(found.unprotectable.contains(&("CLAUDE.md".to_string(), "is a symlink")));
        }
    }

    #[test]
    fn a_full_gitdir_keeps_hooks_config_and_info_of_itself_and_its_submodules() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        for sub in [
            ".git/hooks",
            ".git/info",
            ".git/objects",
            ".git/modules/lib/hooks",
            ".git/modules/lib/info",
            ".git/modules/lib/modules/inner/hooks",
            ".git/modules/vendor/tool/hooks",
            "crates/a/.git/hooks",
            "crates/a/.git/info",
        ] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        for file in [
            ".git/config",
            ".git/index",
            ".git/modules/lib/HEAD",
            ".git/modules/lib/config",
            ".git/modules/lib/modules/inner/HEAD",
            ".git/modules/vendor/tool/HEAD",
            "crates/a/.git/config",
        ] {
            std::fs::write(root.join(file), "x").unwrap();
        }
        #[cfg(unix)]
        {
            std::fs::create_dir_all(root.join("crates/b/.git/hooks")).unwrap();
            std::fs::write(root.join("crates/b/.git/config"), "x").unwrap();
            std::os::unix::fs::symlink(root.join(".git/info"), root.join("crates/b/.git/info"))
                .unwrap();
        }

        let found = protected_paths(&root);

        let mut git: Vec<PathBuf> = found
            .present
            .iter()
            .map(|path| path.strip_prefix(&root).unwrap().to_path_buf())
            .filter(|path| path.iter().any(|part| part == ".git"))
            .collect();
        git.sort();
        let mut expected: Vec<PathBuf> = [
            ".git/config",
            ".git/hooks",
            ".git/info",
            ".git/modules/lib/config",
            ".git/modules/lib/hooks",
            ".git/modules/lib/info",
            // Without config and info, a submodule's gitdir stays read-only whole.
            ".git/modules/lib/modules/inner",
            ".git/modules/vendor/tool",
            "crates/a/.git/config",
            "crates/a/.git/hooks",
            "crates/a/.git/info",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        if cfg!(unix) {
            // A symlinked entry could be replaced, so its gitdir stays read-only whole.
            expected.push(PathBuf::from("crates/b/.git"));
        }
        expected.sort();
        assert_eq!(git, expected);
        assert!(found.unprotectable.iter().all(|(name, _)| name != ".git"));
    }

    #[test]
    fn a_git_file_is_kept_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join(".git"), "gitdir: /elsewhere/.git/worktrees/w\n").unwrap();
        let found = protected_paths(&root);
        assert!(found.present.contains(&root.join(".git")), "{found:?}");
        assert!(found.unprotectable.iter().all(|(name, _)| name != ".git"));
    }

    #[test]
    fn a_symlink_out_of_the_root_is_never_granted() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("notes.md"), "private").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path().join("notes.md"), root.join("MEMORY.md"))
            .unwrap();
        let found = protected_paths(&root);
        assert!(found.present.iter().all(|path| path.starts_with(&root)), "{found:?}");
    }
}
