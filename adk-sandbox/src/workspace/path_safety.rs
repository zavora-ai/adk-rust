//! Path safety utilities for workspace operations.
//!
//! All file paths in sandbox sessions are relative to the workspace root.
//! This module provides validation to reject absolute paths and traversal
//! attempts that would escape the workspace boundary.

use std::path::{Component, Path, PathBuf};

use crate::SandboxError;

/// Validates that a path is relative and does not escape the workspace root.
///
/// The function checks that:
/// - The path is non-empty
/// - The path is not absolute (does not start with `/` or `\`)
/// - The path does not contain `..` components that would escape the root
///
/// Paths with `..` that stay within bounds are allowed (e.g., `a/b/../c`
/// resolves to `a/c` which is still inside the workspace).
///
/// # Errors
///
/// Returns `SandboxError::PathTraversal` if the path is absolute or
/// contains traversal components that would escape the workspace root.
///
/// # Examples
///
/// ```
/// use adk_sandbox::workspace::path_safety::validate_relative_path;
///
/// // Valid paths
/// assert!(validate_relative_path("file.txt").is_ok());
/// assert!(validate_relative_path("src/main.rs").is_ok());
/// assert!(validate_relative_path("a/b/../c/file.txt").is_ok());
/// assert!(validate_relative_path("./file.txt").is_ok());
///
/// // Invalid paths
/// assert!(validate_relative_path("").is_err());
/// assert!(validate_relative_path("/etc/passwd").is_err());
/// assert!(validate_relative_path("../escape").is_err());
/// assert!(validate_relative_path("a/../../escape").is_err());
/// ```
pub fn validate_relative_path(path: &str) -> Result<(), SandboxError> {
    // Reject empty paths
    if path.is_empty() {
        return Err(SandboxError::PathTraversal { path: path.to_string() });
    }

    // Reject absolute paths (Unix `/` or Windows `\` prefix)
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(SandboxError::PathTraversal { path: path.to_string() });
    }

    // Also reject Windows-style drive letters (e.g., "C:\...")
    if path.len() >= 2 {
        let bytes = path.as_bytes();
        if bytes[0].is_ascii_alphabetic() && (bytes[1] == b':') {
            return Err(SandboxError::PathTraversal { path: path.to_string() });
        }
    }

    // Normalize and check for traversal beyond root.
    // We simulate path resolution by tracking depth relative to root.
    // Split on both `/` and `\` to handle mixed separators.
    let mut depth: i32 = 0;

    for component in path.split(['/', '\\']) {
        match component {
            // Empty components from consecutive separators or trailing slash
            "" => continue,
            // Current directory — no-op
            "." => continue,
            // Parent directory — decrease depth
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return Err(SandboxError::PathTraversal { path: path.to_string() });
                }
            }
            // Normal component — increase depth
            _ => {
                depth += 1;
            }
        }
    }

    Ok(())
}

/// Resolves `path` against the workspace `root` and rejects any path that leaves it,
/// lexically or by following a symlink.
///
/// `..` components are resolved lexically, so the operating system never interprets them
/// relative to a symlink. The deepest existing ancestor of the result is then
/// canonicalized, which follows every symlink on the way, and must still be inside the
/// canonical root. A dangling or looping symlink on the path is rejected wherever it
/// points, since a write would follow it and create its target. Symlinks that stay inside
/// the workspace are allowed.
///
/// This is a check, not a lock: a process that can create symlinks inside the workspace
/// can still race it, which is why commands are not confined either.
///
/// # Errors
///
/// Returns `SandboxError::PathTraversal` when the path escapes the root, and
/// `SandboxError::ExecutionFailed` when the root itself cannot be resolved.
pub fn resolve_in_root(root: &Path, path: &str) -> Result<PathBuf, SandboxError> {
    validate_relative_path(path)?;
    let escape = || SandboxError::PathTraversal { path: path.to_string() };
    let root = std::fs::canonicalize(root).map_err(|e| {
        SandboxError::ExecutionFailed(format!("workspace root is not accessible: {e}"))
    })?;
    let mut resolved = root.clone();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(name) => resolved.push(name),
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }

    let mut existing = resolved.as_path();
    loop {
        match std::fs::canonicalize(existing) {
            Ok(canonical) if canonical.starts_with(&root) => return Ok(resolved),
            Ok(_) => return Err(escape()),
            // An entry that exists but does not canonicalize is a dangling or looping symlink.
            Err(_) if std::fs::symlink_metadata(existing).is_ok() => return Err(escape()),
            Err(_) => match existing.parent() {
                Some(parent) if parent.starts_with(&root) => existing = parent,
                _ => return Ok(resolved),
            },
        }
    }
}

/// Writes `content` to `path` inside the workspace `root`, creating parent directories.
///
/// The path is resolved with [`resolve_in_root`]. A symlink that stays inside the
/// workspace is written through its target; on Unix the final component is opened with
/// `O_NOFOLLOW`, so a symlink planted after the check fails the write instead of
/// redirecting it. A file with more than one hard link is replaced, keeping its
/// permissions, rather than truncated: truncating writes through every link, including
/// one outside the workspace.
///
/// # Errors
///
/// Returns `SandboxError::PathTraversal` when the path escapes the root and
/// `SandboxError::ExecutionFailed` when the write fails.
pub async fn write_in_root(root: &Path, path: &str, content: &[u8]) -> Result<(), SandboxError> {
    let resolved = resolve_in_root(root, path)?;
    let failed = |e: std::io::Error| {
        SandboxError::ExecutionFailed(format!("failed to write file '{path}': {e}"))
    };
    if let Some(parent) = resolved.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            SandboxError::ExecutionFailed(format!(
                "failed to create parent directories for '{path}': {e}"
            ))
        })?;
    }
    let is_symlink = tokio::fs::symlink_metadata(&resolved)
        .await
        .is_ok_and(|meta| meta.file_type().is_symlink());
    let target = if is_symlink {
        tokio::fs::canonicalize(&resolved).await.map_err(failed)?
    } else {
        let (Some(parent), Some(name)) = (resolved.parent(), resolved.file_name()) else {
            return Err(SandboxError::PathTraversal { path: path.to_string() });
        };
        tokio::fs::canonicalize(parent).await.map_err(failed)?.join(name)
    };
    let root = tokio::fs::canonicalize(root).await.map_err(failed)?;
    if !target.starts_with(&root) {
        return Err(SandboxError::PathTraversal { path: path.to_string() });
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(meta) = tokio::fs::symlink_metadata(&target).await
            && meta.nlink() > 1
        {
            return replace_file(target, meta.permissions(), content.to_vec())
                .await
                .map_err(failed);
        }
    }

    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(&target).await.map_err(failed)?;
    tokio::io::AsyncWriteExt::write_all(&mut file, content).await.map_err(failed)?;
    tokio::io::AsyncWriteExt::flush(&mut file).await.map_err(failed)
}

/// Writes `content` to a new file beside `target` and renames it over `target`.
#[cfg(unix)]
async fn replace_file(
    target: PathBuf,
    permissions: std::fs::Permissions,
    content: Vec<u8>,
) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || {
        let dir = target.parent().unwrap_or(Path::new("."));
        let mut file = tempfile::NamedTempFile::new_in(dir)?;
        std::io::Write::write_all(&mut file, &content)?;
        file.as_file().set_permissions(permissions)?;
        file.persist(&target).map_err(|e| e.error)?;
        Ok(())
    })
    .await
    .map_err(std::io::Error::other)?
}

/// The file names a unified, context, or git diff can make `patch` open.
///
/// Every line is scanned, not only headers, so a hunk line that looks like a header
/// counts too: that can only reject a patch, never let a file through.
pub(crate) fn patch_target_paths(patch: &str) -> impl Iterator<Item = &str> {
    patch.lines().flat_map(|line| {
        let names: Vec<&str> = if let Some(rest) = line.strip_prefix("diff --git ") {
            rest.split_whitespace().collect()
        } else {
            [
                "--- ",
                "+++ ",
                "*** ",
                "Index: ",
                "rename from ",
                "rename to ",
                "copy from ",
                "copy to ",
            ]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
            .into_iter()
            .collect()
        };
        names.into_iter().filter_map(|name| {
            // A header may carry a timestamp after a tab, and git quotes unusual names.
            let name = name.split('\t').next().unwrap_or_default().trim().trim_matches('"');
            (!name.is_empty() && name != "/dev/null").then_some(name)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_simple_file() {
        assert!(validate_relative_path("file.txt").is_ok());
    }

    #[test]
    fn test_valid_nested_path() {
        assert!(validate_relative_path("src/main.rs").is_ok());
    }

    #[test]
    fn test_valid_dotdot_within_bounds() {
        assert!(validate_relative_path("a/b/../c/file.txt").is_ok());
    }

    #[test]
    fn test_valid_dot_component() {
        assert!(validate_relative_path("./file.txt").is_ok());
    }

    #[test]
    fn test_valid_multiple_dots() {
        assert!(validate_relative_path("./src/./main.rs").is_ok());
    }

    #[test]
    fn test_valid_deep_path() {
        assert!(validate_relative_path("a/b/c/d/e/f.txt").is_ok());
    }

    #[test]
    fn test_invalid_empty_path() {
        let result = validate_relative_path("");
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::PathTraversal { path } => assert_eq!(path, ""),
            other => panic!("expected PathTraversal, got: {other:?}"),
        }
    }

    #[test]
    fn test_invalid_absolute_unix() {
        let result = validate_relative_path("/etc/passwd");
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::PathTraversal { path } => assert_eq!(path, "/etc/passwd"),
            other => panic!("expected PathTraversal, got: {other:?}"),
        }
    }

    #[test]
    fn test_invalid_parent_escape() {
        assert!(validate_relative_path("../escape").is_err());
    }

    #[test]
    fn test_invalid_deep_parent_escape() {
        assert!(validate_relative_path("a/../../escape").is_err());
    }

    #[test]
    fn test_invalid_triple_parent_escape() {
        assert!(validate_relative_path("a/b/../../../escape").is_err());
    }

    #[test]
    fn test_invalid_backslash_absolute() {
        assert!(validate_relative_path("\\Windows\\System32").is_err());
    }

    #[test]
    fn test_invalid_windows_drive() {
        assert!(validate_relative_path("C:\\Users\\file.txt").is_err());
    }

    #[test]
    fn test_valid_dotdot_exact_boundary() {
        // a/b/.. resolves to a/ — still within workspace
        assert!(validate_relative_path("a/b/..").is_ok());
    }

    #[test]
    fn test_valid_multiple_dotdot_within_bounds() {
        // a/b/c/../../d resolves to a/d — still within workspace
        assert!(validate_relative_path("a/b/c/../../d").is_ok());
    }

    #[test]
    fn test_invalid_just_dotdot() {
        assert!(validate_relative_path("..").is_err());
    }

    #[test]
    fn patch_target_paths_cover_every_header_kind() {
        let patch = "\
diff --git a/src/lib.rs b/src/lib.rs
rename from old/name.rs
rename to new/name.rs
Index: index/file.c
--- before.txt\t2026-10-15 10:00:00
+++ after.txt
*** context.txt
--- /dev/null
@@ -1 +1 @@
-gone
+kept
";
        assert_eq!(
            patch_target_paths(patch).collect::<Vec<_>>(),
            [
                "a/src/lib.rs",
                "b/src/lib.rs",
                "old/name.rs",
                "new/name.rs",
                "index/file.c",
                "before.txt",
                "after.txt",
                "context.txt",
            ]
        );
    }
}
