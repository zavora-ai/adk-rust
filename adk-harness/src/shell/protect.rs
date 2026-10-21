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
        if std::fs::canonicalize(&path).is_ok_and(|resolved| resolved.starts_with(root)) {
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
            if !file_type.is_symlink() && is_protected_path(&relative.to_string_lossy()) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The symlink cases are Unix-only: creating a symlink on Windows needs a privilege the
    /// test runner does not hold.
    #[test]
    fn missing_and_symlinked_root_paths_are_unprotectable_and_nested_ones_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        for sub in [".git/hooks", "crates/a/.git", "crates/a/.cargo", "target/x/.git"] {
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
