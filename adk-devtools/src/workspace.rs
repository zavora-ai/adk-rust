//! The sandboxed workspace that scopes every developer-tool operation.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::DevToolError;

/// A workspace roots every file/search/shell operation at a directory and
/// enforces a small capability policy.
///
/// All paths supplied to the tools are resolved relative to [`root`](Self::root)
/// and rejected if they escape it. Mutating operations require
/// [`is_writable`](Self::is_writable); `bash` requires [`bash_allowed`](Self::bash_allowed).
///
/// The workspace also carries a small amount of shared session state — the set
/// of files that have been read — so that `edit_file` can require a prior
/// `read_file` (guarding against blind overwrites).
///
/// `Workspace` is cheap to clone; clones share the read-tracking state.
#[derive(Clone)]
pub struct Workspace {
    root: PathBuf,
    writable: bool,
    allow_bash: bool,
    bash_timeout: Duration,
    max_output_bytes: usize,
    read_tracker: Arc<Mutex<HashSet<PathBuf>>>,
    /// Whether `bash` inherits the parent process environment.
    inherit_env: bool,
    /// Variables passed through when the environment is not inherited.
    env_allowlist: Vec<String>,
}

/// Environment variables `bash` receives by default.
///
/// The parent environment of an agent process routinely holds provider API keys, and a
/// model-directed command could read them with `env`. Only variables tools genuinely
/// need to function are passed through, and none of them is a credential.
pub const DEFAULT_ENV_ALLOWLIST: &[&str] =
    &["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR", "TERM", "USER", "SHELL"];

impl Workspace {
    /// Create a read-write workspace rooted at `root` (bash enabled).
    ///
    /// If `root` exists it is canonicalized so containment checks are robust.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        Self {
            root,
            writable: true,
            allow_bash: true,
            bash_timeout: Duration::from_secs(120),
            max_output_bytes: 1_048_576,
            read_tracker: Arc::new(Mutex::new(HashSet::new())),
            inherit_env: false,
            env_allowlist: DEFAULT_ENV_ALLOWLIST.iter().map(|k| (*k).to_string()).collect(),
        }
    }

    /// Create a read-only workspace (no writes, no bash) — useful for
    /// exploration / plan modes.
    pub fn read_only(root: impl Into<PathBuf>) -> Self {
        let mut ws = Self::new(root);
        ws.writable = false;
        ws.allow_bash = false;
        ws
    }

    /// Set whether mutating file operations are permitted.
    pub fn writable(mut self, yes: bool) -> Self {
        self.writable = yes;
        self
    }

    /// Set whether the `bash` tool is permitted.
    pub fn allow_bash(mut self, yes: bool) -> Self {
        self.allow_bash = yes;
        self
    }

    /// Set the default timeout applied to `bash` commands.
    pub fn bash_timeout(mut self, timeout: Duration) -> Self {
        self.bash_timeout = timeout;
        self
    }

    /// Set the maximum number of bytes captured from a stream before truncation.
    pub fn max_output_bytes(mut self, bytes: usize) -> Self {
        self.max_output_bytes = bytes;
        self
    }

    /// Pass the whole parent environment to `bash`.
    ///
    /// Off by default, because the parent environment of an agent process routinely
    /// holds provider API keys and a model-directed command can read them with `env`.
    /// Enable it only when the commands you run genuinely need the caller's environment
    /// and you accept that exposure.
    #[must_use]
    pub fn inherit_env(mut self, yes: bool) -> Self {
        self.inherit_env = yes;
        self
    }

    /// Replace the variables `bash` receives when the environment is not inherited.
    ///
    /// Defaults to [`DEFAULT_ENV_ALLOWLIST`]. Adding a variable that holds a credential
    /// re-exposes it.
    #[must_use]
    pub fn env_allowlist<I, S>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.env_allowlist = keys.into_iter().map(Into::into).collect();
        self
    }

    /// Whether `bash` inherits the parent environment.
    pub fn inherits_env(&self) -> bool {
        self.inherit_env
    }

    /// The variables `bash` receives, resolved from the current process.
    ///
    /// Empty when the environment is inherited, in which case the caller must not clear
    /// it.
    pub fn bash_env(&self) -> Vec<(String, String)> {
        if self.inherit_env {
            return Vec::new();
        }
        self.env_allowlist
            .iter()
            .filter_map(|key| std::env::var(key).ok().map(|value| (key.clone(), value)))
            .collect()
    }

    /// The workspace root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether mutating file operations are permitted.
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Whether the `bash` tool is permitted.
    pub fn bash_allowed(&self) -> bool {
        self.allow_bash
    }

    /// The default `bash` timeout.
    pub fn bash_timeout_value(&self) -> Duration {
        self.bash_timeout
    }

    /// The output-capture cap.
    pub fn max_output(&self) -> usize {
        self.max_output_bytes
    }

    /// Resolve a user-supplied path against the root, rejecting any path that
    /// escapes it (lexically). The target need not exist yet.
    pub fn resolve(&self, path: &str) -> Result<PathBuf, DevToolError> {
        let requested = Path::new(path);
        let joined = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            self.root.join(requested)
        };
        let normalized = normalize(&joined);
        if !normalized.starts_with(&self.root) {
            return Err(DevToolError::PathEscape(path.to_string()));
        }
        // A lexical check alone is not containment: a symlink sitting lexically
        // under the root can point anywhere, and ordinary file I/O follows it.
        self.reject_symlink_escape(&normalized, path)?;
        Ok(normalized)
    }

    /// Rejects a path that reaches outside the root by following a symlink.
    ///
    /// The deepest existing ancestor of the target is canonicalized, which resolves
    /// every symlink along the way, and the result must still be inside the root.
    /// That covers a symlinked final component and a symlinked parent directory, so
    /// creation through a redirected directory is refused as well. A symlink whose
    /// target stays inside the workspace is allowed, since repositories legitimately
    /// contain internal links.
    ///
    /// This is a check, not a lock. A symlink swapped between this check and the
    /// subsequent open would still be followed; closing that window needs
    /// descriptor-relative traversal with platform no-follow semantics.
    fn reject_symlink_escape(
        &self,
        normalized: &Path,
        requested: &str,
    ) -> Result<(), DevToolError> {
        let mut existing = normalized;
        loop {
            match std::fs::canonicalize(existing) {
                Ok(canonical) => {
                    if !canonical.starts_with(&self.root) {
                        return Err(DevToolError::PathEscape(requested.to_string()));
                    }
                    return Ok(());
                }
                // The path does not exist yet, so step up to what does. A component
                // that does not exist cannot redirect anything.
                Err(_) => match existing.parent() {
                    Some(parent) if parent.starts_with(&self.root) => existing = parent,
                    _ => return Ok(()),
                },
            }
        }
    }

    /// Render a path relative to the root for display (falls back to the full path).
    pub fn display(&self, path: &Path) -> String {
        path.strip_prefix(&self.root).unwrap_or(path).display().to_string()
    }

    /// Record that a file has been read this session.
    pub(crate) fn mark_read(&self, path: &Path) {
        if let Ok(mut set) = self.read_tracker.lock() {
            set.insert(path.to_path_buf());
        }
    }

    /// Whether a file has been read this session.
    pub(crate) fn was_read(&self, path: &Path) -> bool {
        self.read_tracker.lock().map(|set| set.contains(path)).unwrap_or(false)
    }
}

/// Lexically normalize a path, resolving `.` and `..` without touching the
/// filesystem (so non-existent targets still normalize).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_escape() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path());
        assert!(ws.resolve("../etc/passwd").is_err());
        assert!(ws.resolve("ok/file.rs").is_ok());
    }

    #[test]
    fn read_tracking() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path());
        let p = ws.resolve("a.txt").unwrap();
        assert!(!ws.was_read(&p));
        ws.mark_read(&p);
        assert!(ws.was_read(&p));
    }
}
