//! The sandboxed workspace that scopes every developer-tool operation.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncWriteExt;

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
    /// Create a read-write workspace rooted at `root`, with `bash` disabled.
    ///
    /// `bash` runs model-written commands on the host, outside the path containment the file
    /// tools enforce, so it is off until [`allow_bash`](Self::allow_bash) enables it. If `root`
    /// exists it is canonicalized so containment checks are robust.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_devtools::Workspace;
    ///
    /// let files_only = Workspace::new("./my-repo");
    /// assert!(!files_only.bash_allowed());
    ///
    /// let with_shell = Workspace::new("./my-repo").allow_bash(true);
    /// assert!(with_shell.bash_allowed());
    /// ```
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        Self {
            root,
            writable: true,
            allow_bash: false,
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

    /// Set whether the `bash` tool is permitted. Off by default.
    ///
    /// Commands run with the workspace root as their working directory, which is not a sandbox:
    /// they can reach absolute paths and the network.
    pub fn allow_bash(mut self, yes: bool) -> Self {
        self.allow_bash = yes;
        self
    }

    /// Set the timeout applied to `bash` commands, and the most a call may request.
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
    /// A dangling or looping symlink anywhere on the path is refused, wherever it
    /// points: canonicalization cannot see through it, and a write follows it and
    /// creates its target.
    ///
    /// This is a check, not a lock. [`write_contained`](Self::write_contained)
    /// narrows the window for a symlink swapped in before the open.
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
                Err(_) => {
                    // An entry that exists but does not canonicalize is a symlink whose
                    // target is missing or loops back on itself.
                    if std::fs::symlink_metadata(existing).is_ok() {
                        return Err(DevToolError::PathEscape(requested.to_string()));
                    }
                    // Nothing exists here yet, so step up to what does. A component
                    // that does not exist cannot redirect anything.
                    match existing.parent() {
                        Some(parent) if parent.starts_with(&self.root) => existing = parent,
                        _ => return Ok(()),
                    }
                }
            }
        }
    }

    /// Writes `contents` to a path returned by [`resolve`](Self::resolve), creating
    /// or truncating the file.
    ///
    /// The canonical parent is re-checked against the root immediately before the
    /// open, and on Unix the final component is opened with `O_NOFOLLOW`, so a
    /// symlink planted after `resolve` fails the open instead of redirecting the
    /// write. An existing symlink that `resolve` accepted, which points inside the
    /// workspace, is written through its canonical target.
    pub(crate) async fn write_contained(
        &self,
        resolved: &Path,
        contents: &[u8],
    ) -> Result<(), DevToolError> {
        let escape = || DevToolError::PathEscape(self.display(resolved));
        let is_symlink = tokio::fs::symlink_metadata(resolved)
            .await
            .is_ok_and(|meta| meta.file_type().is_symlink());
        let target = if is_symlink {
            tokio::fs::canonicalize(resolved).await.map_err(|_| escape())?
        } else {
            let (Some(parent), Some(name)) = (resolved.parent(), resolved.file_name()) else {
                return Err(escape());
            };
            tokio::fs::canonicalize(parent).await?.join(name)
        };
        if !target.starts_with(&self.root) {
            return Err(escape());
        }

        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        let mut file = options.open(&target).await?;
        file.write_all(contents).await?;
        file.flush().await?;
        Ok(())
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

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_planted_after_resolve_is_not_followed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("existing.txt"), "host").unwrap();
        let ws = Workspace::new(&root);

        for (name, target) in
            [("dangling", outside.join("planted.txt")), ("existing", outside.join("existing.txt"))]
        {
            let resolved = ws.resolve(name).expect("the path does not exist yet");
            std::os::unix::fs::symlink(&target, root.join(name)).unwrap();

            let result = ws.write_contained(&resolved, b"payload").await;

            assert!(
                matches!(result, Err(DevToolError::PathEscape(_))),
                "a {name} link planted after resolve was followed: {result:?}"
            );
        }
        assert!(!outside.join("planted.txt").exists());
        assert_eq!(std::fs::read_to_string(outside.join("existing.txt")).unwrap(), "host");
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
