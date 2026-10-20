//! The sandboxed workspace that scopes every developer-tool operation.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::ToolContext;
use tokio::io::AsyncWriteExt;

use crate::error::DevToolError;

/// A workspace roots every file/search/shell operation at a directory and
/// enforces a small capability policy.
///
/// All paths supplied to the tools are resolved relative to [`root`](Self::root)
/// and rejected if they escape it. Mutating operations require
/// [`is_writable`](Self::is_writable); `bash` requires [`bash_allowed`](Self::bash_allowed).
///
/// The workspace also records which files have been read, so that `edit_file`
/// can require a prior `read_file` (guarding against blind overwrites). The record
/// is kept per session and agent, taken from the tool call's context, so one
/// session or agent reading a file does not let another edit it.
///
/// `Workspace` is cheap to clone. Clones share the read record, which stays
/// separated by session and agent; calls whose context names no session share one
/// record.
#[derive(Clone)]
pub struct Workspace {
    root: PathBuf,
    writable: bool,
    allow_bash: bool,
    bash_timeout: Duration,
    max_output_bytes: usize,
    read_tracker: Arc<Mutex<ReadTracker>>,
    /// Whether `bash` inherits the parent process environment.
    inherit_env: bool,
    /// Variables passed through when the environment is not inherited.
    env_allowlist: Vec<String>,
    /// Whether `bash` kills the command's process group once the shell exits.
    kill_group_on_exit: bool,
    /// Files that `grep` and `glob` skip and `read_file` refuses.
    exclude: Vec<glob::Pattern>,
}

/// Environment variables `bash` receives by default.
///
/// The parent environment of an agent process routinely holds provider API keys, and a
/// model-directed command could read them with `env`. Only variables tools genuinely
/// need to function are passed through, and none of them is a credential.
pub const DEFAULT_ENV_ALLOWLIST: &[&str] =
    &["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR", "TERM", "USER", "SHELL"];

/// File patterns a new [`Workspace`] excludes: environment files, private keys and
/// certificate bundles.
///
/// `grep` and `glob` skip matching files and `read_file` refuses them with
/// [`DevToolError::Excluded`]. A pattern matches a path relative to the root, or any
/// one of its components, ignoring case. Implements spec 3.1 §10 D5.
///
/// # Example
///
/// ```rust
/// use adk_devtools::{DEFAULT_EXCLUDE_GLOBS, Workspace};
///
/// assert!(DEFAULT_EXCLUDE_GLOBS.contains(&".env"));
/// let ws = Workspace::new("./my-repo");
/// assert!(ws.is_excluded(std::path::Path::new("config/.env")));
/// ```
pub const DEFAULT_EXCLUDE_GLOBS: &[&str] =
    &[".env", ".env.*", "*.pem", "*.key", "id_rsa*", "id_ed25519*", "*.p12", "*.pfx"];

/// How many (session, agent) read records a workspace keeps before evicting the
/// least recently used one.
const READ_SCOPE_CAPACITY: usize = 1024;

/// Exclusion patterns match regardless of case, since `.ENV` names `.env` on a
/// case-insensitive file system, and `*` never crosses a `/`.
const EXCLUDE_MATCH: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: false,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

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
            read_tracker: Arc::new(Mutex::new(ReadTracker::default())),
            inherit_env: false,
            env_allowlist: DEFAULT_ENV_ALLOWLIST.iter().map(|k| (*k).to_string()).collect(),
            kill_group_on_exit: true,
            exclude: compile_excludes(DEFAULT_EXCLUDE_GLOBS.iter().map(|glob| (*glob).to_string())),
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

    /// Set whether `bash` kills the rest of the command's process group once the
    /// shell exits. On by default.
    ///
    /// With it on, a process the command leaves running in the background — `sleep 300 &`,
    /// a dev server — is killed when the call returns, and the call does not wait for
    /// it to close its output. Turn it off only when commands must leave processes
    /// running; a timeout or a cancelled call still kills the group. Implements spec 3.1
    /// §10 D3.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_devtools::Workspace;
    ///
    /// let ws = Workspace::new("./my-repo").allow_bash(true);
    /// assert!(ws.kills_bash_group_on_exit());
    ///
    /// let keeps_servers = ws.bash_kill_group_on_exit(false);
    /// assert!(!keeps_servers.kills_bash_group_on_exit());
    /// ```
    #[must_use]
    pub fn bash_kill_group_on_exit(mut self, yes: bool) -> Self {
        self.kill_group_on_exit = yes;
        self
    }

    /// Whether `bash` kills the command's process group once the shell exits.
    ///
    /// Set with [`bash_kill_group_on_exit`](Self::bash_kill_group_on_exit); spec 3.1 §10 D3.
    pub fn kills_bash_group_on_exit(&self) -> bool {
        self.kill_group_on_exit
    }

    /// Replace the file patterns `grep` and `glob` skip and `read_file` refuses.
    ///
    /// Defaults to [`DEFAULT_EXCLUDE_GLOBS`]; an empty list turns exclusion off. Each
    /// pattern is a glob matched, ignoring case, against the path relative to the
    /// root and against each of its components, so `.env` also excludes
    /// `config/.env` and `secrets/**` everything under `secrets/`. A pattern that is
    /// not a valid glob matches its literal text. Implements spec 3.1 §10 D5.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::path::Path;
    ///
    /// use adk_devtools::Workspace;
    ///
    /// let ws = Workspace::new("./my-repo").exclude_globs(["secrets/**".to_string()]);
    /// assert!(ws.is_excluded(Path::new("secrets/prod/token.txt")));
    /// assert!(!ws.is_excluded(Path::new(".env")));
    ///
    /// let everything = Workspace::new("./my-repo").exclude_globs([]);
    /// assert!(!everything.is_excluded(Path::new(".env")));
    /// ```
    #[must_use]
    pub fn exclude_globs(mut self, globs: impl IntoIterator<Item = String>) -> Self {
        self.exclude = compile_excludes(globs);
        self
    }

    /// Whether `path` matches one of the exclusion patterns.
    ///
    /// `path` is relative to the root, or an absolute path inside it. The check is
    /// lexical; `read_file` also checks the path a symlink resolves to. A harness
    /// uses it to require approval before a read. Implements spec 3.1 §10 D5.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::path::Path;
    ///
    /// use adk_devtools::Workspace;
    ///
    /// let ws = Workspace::new("./my-repo");
    /// assert!(ws.is_excluded(Path::new("deploy/server.KEY")));
    /// assert!(!ws.is_excluded(Path::new("src/keys.rs")));
    /// ```
    pub fn is_excluded(&self, path: &Path) -> bool {
        if self.exclude.is_empty() {
            return false;
        }
        let relative = path.strip_prefix(&self.root).unwrap_or(path);
        self.exclude.iter().any(|pattern| {
            pattern.matches_path_with(relative, EXCLUDE_MATCH)
                || relative.components().any(|component| match component {
                    Component::Normal(name) => {
                        pattern.matches_with(&name.to_string_lossy(), EXCLUDE_MATCH)
                    }
                    Component::Prefix(_)
                    | Component::RootDir
                    | Component::CurDir
                    | Component::ParentDir => false,
                })
        })
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
    /// workspace, is written through its canonical target. A file with more than one
    /// hard link is replaced, keeping its permissions, rather than truncated:
    /// truncating writes through every link, including one outside the workspace.
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

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if let Ok(meta) = tokio::fs::symlink_metadata(&target).await
                && meta.nlink() > 1
            {
                return replace_file(&target, meta.permissions(), contents).await;
            }
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

    /// Record that the session and agent behind `ctx` have read a file.
    pub(crate) fn mark_read(&self, ctx: &dyn ToolContext, path: &Path) {
        if let Ok(mut tracker) = self.read_tracker.lock() {
            tracker.files(ReadScope::of(ctx)).insert(path.to_path_buf());
        }
    }

    /// Whether the session and agent behind `ctx` have read a file.
    pub(crate) fn was_read(&self, ctx: &dyn ToolContext, path: &Path) -> bool {
        self.read_tracker
            .lock()
            .map(|mut tracker| tracker.files(ReadScope::of(ctx)).contains(path))
            .unwrap_or(false)
    }
}

/// Compiles exclusion globs; a pattern that does not parse matches its literal text.
fn compile_excludes(globs: impl IntoIterator<Item = String>) -> Vec<glob::Pattern> {
    globs
        .into_iter()
        .map(|glob| {
            glob::Pattern::new(&glob).unwrap_or_else(|error| {
                tracing::warn!(
                    exclude.pattern = %glob,
                    error = %error,
                    "exclusion pattern is not a valid glob; matching it literally"
                );
                glob::Pattern::new(&glob::Pattern::escape(&glob))
                    .expect("an escaped pattern is a valid glob")
            })
        })
        .collect()
}

/// The session and agent a read is recorded for.
///
/// Session ids are unique only within an app and user, so both are part of the key.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ReadScope {
    app: String,
    user: String,
    session: String,
    agent: String,
}

impl ReadScope {
    /// The scope of the call behind `ctx`, or `None` when it names no session.
    fn of(ctx: &dyn ToolContext) -> Option<Self> {
        let session = ctx.session_id();
        if session.is_empty() {
            return None;
        }
        Some(Self {
            app: ctx.app_name().to_string(),
            user: ctx.user_id().to_string(),
            session: session.to_string(),
            agent: ctx.agent_name().to_string(),
        })
    }
}

/// Files read, per (session, agent) scope, with least-recently-used eviction.
#[derive(Default)]
struct ReadTracker {
    /// Reads by calls that name no session.
    unscoped: HashSet<PathBuf>,
    /// Reads per scope, with the tick of the scope's last use.
    scoped: HashMap<ReadScope, (u64, HashSet<PathBuf>)>,
    tick: u64,
}

impl ReadTracker {
    /// The read set of `scope`, created (evicting the least recently used scope at
    /// capacity) if absent.
    fn files(&mut self, scope: Option<ReadScope>) -> &mut HashSet<PathBuf> {
        let Some(scope) = scope else {
            return &mut self.unscoped;
        };
        self.tick += 1;
        if !self.scoped.contains_key(&scope)
            && self.scoped.len() >= READ_SCOPE_CAPACITY
            && let Some(oldest) = self
                .scoped
                .iter()
                .min_by_key(|(_, (used, _))| *used)
                .map(|(oldest, _)| oldest.clone())
        {
            self.scoped.remove(&oldest);
        }
        let entry = self.scoped.entry(scope).or_default();
        entry.0 = self.tick;
        &mut entry.1
    }
}

/// Writes `contents` to a new file beside `target` and renames it over `target`.
#[cfg(unix)]
async fn replace_file(
    target: &Path,
    permissions: std::fs::Permissions,
    contents: &[u8],
) -> Result<(), DevToolError> {
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let temp = target.with_file_name(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temp)
            .await?;
        file.write_all(contents).await?;
        file.flush().await?;
        file.set_permissions(permissions).await?;
        tokio::fs::rename(&temp, target).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    Ok(result?)
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

    #[cfg(unix)]
    #[tokio::test]
    async fn writing_a_hard_linked_file_leaves_the_other_link_unchanged() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let outside = temp.path().join("outside.txt");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&outside, "host").unwrap();
        std::fs::hard_link(&outside, root.join("linked.txt")).unwrap();
        std::fs::set_permissions(root.join("linked.txt"), std::fs::Permissions::from_mode(0o640))
            .unwrap();
        let ws = Workspace::new(&root);

        let resolved = ws.resolve("linked.txt").unwrap();
        ws.write_contained(&resolved, b"rewritten").await.unwrap();

        assert_eq!(std::fs::read_to_string(root.join("linked.txt")).unwrap(), "rewritten");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "host");
        let mode = std::fs::metadata(root.join("linked.txt")).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o640);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "no temporary file is left");
    }

    /// A tool context that names a session and an agent.
    struct ScopeCtx {
        session: String,
        agent: String,
    }

    impl ScopeCtx {
        fn new(session: &str, agent: &str) -> Self {
            Self { session: session.to_string(), agent: agent.to_string() }
        }
    }

    #[async_trait::async_trait]
    impl adk_core::ReadonlyContext for ScopeCtx {
        fn invocation_id(&self) -> &str {
            "invocation"
        }
        fn agent_name(&self) -> &str {
            &self.agent
        }
        fn user_id(&self) -> &str {
            "user"
        }
        fn app_name(&self) -> &str {
            "app"
        }
        fn session_id(&self) -> &str {
            &self.session
        }
        fn branch(&self) -> &str {
            ""
        }
        fn user_content(&self) -> &adk_core::Content {
            static CONTENT: std::sync::OnceLock<adk_core::Content> = std::sync::OnceLock::new();
            CONTENT.get_or_init(|| adk_core::Content::new("user").with_text("hi"))
        }
    }

    #[async_trait::async_trait]
    impl adk_core::CallbackContext for ScopeCtx {
        fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
            None
        }
    }

    #[async_trait::async_trait]
    impl ToolContext for ScopeCtx {
        fn function_call_id(&self) -> &str {
            "call"
        }
        fn actions(&self) -> adk_core::EventActions {
            adk_core::EventActions::default()
        }
        fn set_actions(&self, _actions: adk_core::EventActions) {}
        async fn search_memory(
            &self,
            _query: &str,
        ) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
            Ok(vec![])
        }
    }

    #[test]
    fn read_tracking() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path());
        let ctx = ScopeCtx::new("session", "agent");
        let p = ws.resolve("a.txt").unwrap();
        assert!(!ws.was_read(&ctx, &p));
        ws.mark_read(&ctx, &p);
        assert!(ws.was_read(&ctx, &p));
    }

    #[test]
    fn clones_keep_reads_apart_by_session_and_agent() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path());
        let other = ws.clone();
        let p = ws.resolve("a.txt").unwrap();

        ws.mark_read(&ScopeCtx::new("one", "coder"), &p);

        assert!(other.was_read(&ScopeCtx::new("one", "coder"), &p), "clones share a scope");
        assert!(!other.was_read(&ScopeCtx::new("two", "coder"), &p), "another session read it");
        assert!(!other.was_read(&ScopeCtx::new("one", "reviewer"), &p), "another agent read it");
    }

    #[test]
    fn calls_without_a_session_share_one_record() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path());
        let p = ws.resolve("a.txt").unwrap();

        ws.mark_read(&ScopeCtx::new("", "first"), &p);

        assert!(ws.clone().was_read(&ScopeCtx::new("", "second"), &p));
        assert!(!ws.was_read(&ScopeCtx::new("session", "first"), &p));
    }

    #[test]
    fn the_least_recently_used_scope_is_evicted_at_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path());
        let p = ws.resolve("a.txt").unwrap();
        for session in 0..READ_SCOPE_CAPACITY {
            ws.mark_read(&ScopeCtx::new(&format!("s{session}"), "agent"), &p);
        }
        // Touch the oldest scope so the second oldest is the one evicted.
        assert!(ws.was_read(&ScopeCtx::new("s0", "agent"), &p));

        ws.mark_read(&ScopeCtx::new("newest", "agent"), &p);

        assert!(
            ws.was_read(&ScopeCtx::new("s0", "agent"), &p),
            "a recently used scope was evicted"
        );
        assert!(!ws.was_read(&ScopeCtx::new("s1", "agent"), &p), "the oldest scope was kept");
        let scopes = ws.read_tracker.lock().unwrap().scoped.len();
        assert_eq!(scopes, READ_SCOPE_CAPACITY);
    }

    #[test]
    fn secret_patterns_are_excluded_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path());
        for secret in [
            ".env",
            "config/.env",
            ".env.production",
            "certs/server.pem",
            "tls.KEY",
            ".ssh/id_rsa",
            "keys/id_rsa.pub",
            "id_ed25519",
            "bundle.p12",
            "bundle.pfx",
            ".ENV",
        ] {
            assert!(ws.is_excluded(Path::new(secret)), "{secret} was not excluded");
            assert!(ws.is_excluded(&dir.path().join(secret)), "absolute {secret} was not excluded");
        }
        for ordinary in ["src/main.rs", "environment.md", "keys.rs", "README.md", ".envrc"] {
            assert!(!ws.is_excluded(Path::new(ordinary)), "{ordinary} was excluded");
        }
    }

    #[test]
    fn exclusion_patterns_can_be_replaced_or_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let custom = Workspace::new(dir.path()).exclude_globs(["secrets/**".to_string()]);
        assert!(custom.is_excluded(Path::new("secrets/prod/token.txt")));
        assert!(!custom.is_excluded(Path::new(".env")));

        let cleared = Workspace::new(dir.path()).exclude_globs([]);
        assert!(!cleared.is_excluded(Path::new(".env")));

        let literal = Workspace::new(dir.path()).exclude_globs(["[unclosed".to_string()]);
        assert!(literal.is_excluded(Path::new("dir/[unclosed")));
    }
}
