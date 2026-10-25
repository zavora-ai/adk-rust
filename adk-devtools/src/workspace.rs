//! The sandboxed workspace that scopes every developer-tool operation.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::ToolContext;
#[cfg(not(any(unix, windows)))]
use tokio::io::AsyncWriteExt;

#[cfg(windows)]
use crate::beneath::write_beneath;
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

/// The most bytes of a `bash` command's output its output file keeps unless
/// [`Workspace::max_output_bytes`] says otherwise: 64 MiB (spec 3.1 §10 D10).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_devtools::DEFAULT_MAX_OUTPUT_BYTES, 64 * 1024 * 1024);
/// ```
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

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
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
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

    /// Set the most bytes of a `bash` command's output an
    /// [`OutputStore`](crate::OutputStore) file keeps: the disk bound (default
    /// [`DEFAULT_MAX_OUTPUT_BYTES`], 64 MiB; spec 3.1 §10 D10). The result itself carries at
    /// most [`INLINE_OUTPUT_CHARS`](crate::INLINE_OUTPUT_CHARS) characters whatever this
    /// is, and memory holds only the start and end of each stream.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_devtools::Workspace;
    ///
    /// let ws = Workspace::new(".").allow_bash(true).max_output_bytes(8 * 1024 * 1024);
    /// assert_eq!(ws.max_output(), 8 * 1024 * 1024);
    /// ```
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

    /// The most bytes of a command's output its output file keeps.
    pub fn max_output(&self) -> usize {
        self.max_output_bytes
    }

    /// Resolve a user-supplied path against the root, rejecting any path that
    /// escapes it, lexically or through a symlink. The target need not exist yet.
    ///
    /// The returned path is the lexical one. The file tools open the canonical target
    /// the same check finds, following no symlink on it, so a link swapped onto the path
    /// after the check fails the call (spec 3.1 §10 D4).
    pub fn resolve(&self, path: &str) -> Result<PathBuf, DevToolError> {
        self.resolve_target(path).map(|(normalized, _)| normalized)
    }

    /// Resolves `path` as [`resolve`](Self::resolve) does and also returns the canonical
    /// target the check found: where every symlink on the path leads, with the components
    /// that do not exist yet appended. The file tools open that target and nothing else
    /// (spec 3.1 §10 D4).
    pub(crate) fn resolve_target(&self, path: &str) -> Result<(PathBuf, PathBuf), DevToolError> {
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
        let target = self.contained_target(&normalized, path)?;
        Ok((normalized, target))
    }

    /// Returns the canonical target of `normalized`, rejecting one that reaches outside
    /// the root by following a symlink.
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
    /// This is a check, not a lock: the file tools open the returned target without
    /// following any symlink, so a link that appears after the check fails the open.
    fn contained_target(
        &self,
        normalized: &Path,
        requested: &str,
    ) -> Result<PathBuf, DevToolError> {
        let mut existing = normalized;
        loop {
            match std::fs::canonicalize(existing) {
                Ok(canonical) => {
                    if !canonical.starts_with(&self.root) {
                        return Err(DevToolError::PathEscape(requested.to_string()));
                    }
                    // What lies below the deepest existing ancestor does not exist yet.
                    let missing = normalized.strip_prefix(existing).unwrap_or(Path::new(""));
                    return Ok(if missing.as_os_str().is_empty() {
                        canonical
                    } else {
                        canonical.join(missing)
                    });
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
                        _ => return Ok(normalized.to_path_buf()),
                    }
                }
            }
        }
    }

    /// Reads the UTF-8 file at `target`, a canonical target returned by
    /// [`resolve_target`](Self::resolve_target), following no symlink on it.
    ///
    /// On Unix the file is opened beneath the root without following a symlink on any
    /// component, so a link swapped onto the path after the check fails the read; and a
    /// file with more than one hard link is refused, since another link to it can lie
    /// outside the workspace (spec 3.1 §10 D4, §9.2 I28). On Windows the opened handle
    /// is refused when it is a symlink or a junction, or when its final path is not the
    /// checked target, and the same hard-link refusal applies (spec 3.2 §10 W6).
    pub(crate) async fn read_contained(&self, target: &Path) -> Result<String, DevToolError> {
        let bytes = self.read_contained_bytes(target).await?;
        // The same error `read_to_string` reports, on every platform.
        String::from_utf8(bytes).map_err(|_| {
            DevToolError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stream did not contain valid UTF-8",
            ))
        })
    }

    /// Reads the bytes of the file at `target` under the rules of
    /// [`read_contained`](Self::read_contained): the same no-follow open and hard-link
    /// refusal, for content that is not text (spec 3.1 §10 D6, §9.2 I55).
    pub(crate) async fn read_contained_bytes(
        &self,
        target: &Path,
    ) -> Result<Vec<u8>, DevToolError> {
        #[cfg(any(unix, windows))]
        {
            #[cfg(unix)]
            use std::io::Read as _;
            #[cfg(unix)]
            use std::os::unix::fs::MetadataExt as _;

            let relative = self.beneath_root(target)?;
            let root = self.root.clone();
            #[cfg(unix)]
            let read =
                tokio::task::spawn_blocking(move || -> std::io::Result<Result<Vec<u8>, u64>> {
                    let dir = crate::beneath::open_root(&root)?;
                    let opened = crate::beneath::open_beneath(&dir, &relative, libc::O_RDONLY, 0)?;
                    let mut file = std::fs::File::from(opened);
                    let meta = file.metadata()?;
                    if meta.is_file() && meta.nlink() > 1 {
                        return Ok(Err(meta.nlink()));
                    }
                    let mut contents = Vec::new();
                    file.read_to_end(&mut contents)?;
                    Ok(Ok(contents))
                })
                .await
                .map_err(|error| {
                    DevToolError::Other(format!("the read did not complete: {error}"))
                })?;
            #[cfg(windows)]
            let read =
                tokio::task::spawn_blocking(move || crate::beneath::read_beneath(&root, &relative))
                    .await
                    .map_err(|error| {
                        DevToolError::Other(format!("the read did not complete: {error}"))
                    })?;
            match read {
                Ok(Ok(contents)) => Ok(contents),
                Ok(Err(links)) => Err(DevToolError::Other(format!(
                    "'{}' has {links} hard links, and another link to it can lie outside the \
                     workspace, so the file tools do not read it; copy it to a new file to read it",
                    self.display(target)
                ))),
                Err(error) => Err(self.open_error(target, error)),
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(tokio::fs::read(target).await?)
        }
    }

    /// Writes `contents` to `target`, a canonical target returned by
    /// [`resolve_target`](Self::resolve_target), creating the file and its missing
    /// parent directories or truncating it.
    ///
    /// On Unix the parent directory is opened by descriptor beneath the root, creating
    /// what is missing, and the file relative to it, following no symlink on any
    /// component: a symlink planted anywhere on the path after the check fails the
    /// write instead of redirecting it (spec 3.1 §10 D4). A file with more than one
    /// hard link is replaced, keeping its permissions, rather than truncated:
    /// truncating writes through every link, including one outside the workspace.
    ///
    /// On Windows each directory on the path is opened, or created, without following a
    /// link on it and held open until the write ends, and the file is opened without
    /// truncation; the handle is refused when it is a symlink or a junction or when its
    /// final path is not the checked target, and it is truncated only after those checks
    /// (spec 3.2 §10 W6).
    pub(crate) async fn write_contained(
        &self,
        target: &Path,
        contents: &[u8],
    ) -> Result<(), DevToolError> {
        #[cfg(any(unix, windows))]
        {
            let relative = self.beneath_root(target)?;
            let Some(name) = relative.file_name().map(std::ffi::OsStr::to_os_string) else {
                return Err(std::io::Error::from(std::io::ErrorKind::IsADirectory).into());
            };
            let root = self.root.clone();
            let contents = contents.to_vec();
            tokio::task::spawn_blocking(move || write_beneath(&root, &relative, &name, &contents))
                .await
                .map_err(|error| {
                    DevToolError::Other(format!("the write did not complete: {error}"))
                })?
                .map_err(|error| self.open_error(target, error))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let escape = || DevToolError::PathEscape(self.display(target));
            let (Some(parent), Some(name)) = (target.parent(), target.file_name()) else {
                return Err(escape());
            };
            tokio::fs::create_dir_all(parent).await?;
            if !tokio::fs::canonicalize(parent).await?.join(name).starts_with(&self.root) {
                return Err(escape());
            }
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(target)
                .await?;
            file.write_all(contents).await?;
            file.flush().await?;
            Ok(())
        }
    }

    /// `target` relative to the root, which every target the check returns lies under.
    #[cfg(any(unix, windows))]
    fn beneath_root(&self, target: &Path) -> Result<PathBuf, DevToolError> {
        target
            .strip_prefix(&self.root)
            .map(Path::to_path_buf)
            .map_err(|_| DevToolError::PathEscape(self.display(target)))
    }

    /// Reports an open that refused a symlink as an escape, and any other failure as I/O.
    #[cfg(any(unix, windows))]
    fn open_error(&self, target: &Path, error: std::io::Error) -> DevToolError {
        if crate::beneath::is_symlink_refusal(&error) {
            DevToolError::PathEscape(self.display(target))
        } else {
            DevToolError::Io(error)
        }
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

/// Writes `contents` to the entry `name` of the directory `relative` beneath `root`
/// (spec 3.1 §10 D4).
///
/// The parent is opened by descriptor, creating missing directories, and the file
/// relative to it, all without following a symlink. The file is truncated only after
/// its link count is known, so a hard-linked file is replaced instead.
#[cfg(unix)]
fn write_beneath(
    root: &Path,
    relative: &Path,
    name: &std::ffi::OsStr,
    contents: &[u8],
) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::MetadataExt as _;

    let dir = match crate::beneath::open_root(root) {
        // The root is trusted, so a workspace whose root does not exist yet creates it.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(root)?;
            crate::beneath::open_root(root)?
        }
        result => result?,
    };
    let parent_path = relative.parent().unwrap_or(Path::new(""));
    let flags = libc::O_RDONLY | libc::O_DIRECTORY;
    let parent = match crate::beneath::open_beneath(&dir, parent_path, flags, 0) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::beneath::create_dirs_beneath(&dir, parent_path)?
        }
        result => result?,
    };
    let mut file =
        crate::beneath::open_entry(&parent, name, libc::O_WRONLY | libc::O_CREAT, 0o666)?;
    let meta = file.metadata()?;
    if meta.is_file() && meta.nlink() > 1 {
        drop(file);
        return replace_entry(&parent, name, meta.permissions(), contents);
    }
    if meta.is_file() {
        file.set_len(0)?;
    }
    file.write_all(contents)?;
    file.flush()
}

/// Writes `contents` to a new entry beside `name` in `parent` and renames it over
/// `name`.
#[cfg(unix)]
fn replace_entry(
    parent: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
    permissions: std::fs::Permissions,
    contents: &[u8],
) -> std::io::Result<()> {
    use std::io::Write as _;

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let temp = std::ffi::OsString::from(format!(
        ".{}.{}.{nanos}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let result = (|| {
        let mut file = crate::beneath::open_entry(
            parent,
            &temp,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        file.write_all(contents)?;
        file.set_permissions(permissions)?;
        crate::beneath::rename_entry(parent, &temp, name)
    })();
    if result.is_err() {
        let _ = crate::beneath::remove_entry(parent, &temp);
    }
    result
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

    #[cfg(any(unix, windows))]
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
            let Some(()) = link(&target, &root.join(name)) else { return };

            let result = ws.write_contained(&resolved, b"payload").await;

            assert!(
                matches!(result, Err(DevToolError::PathEscape(_))),
                "a {name} link planted after resolve was followed: {result:?}"
            );
        }
        assert!(!outside.join("planted.txt").exists());
        assert_eq!(std::fs::read_to_string(outside.join("existing.txt")).unwrap(), "host");
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn writing_a_hard_linked_file_leaves_the_other_link_unchanged() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let outside = temp.path().join("outside.txt");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&outside, "host").unwrap();
        std::fs::hard_link(&outside, root.join("linked.txt")).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(root.join("linked.txt"), std::fs::Permissions::from_mode(0o640))
            .unwrap();
        let ws = Workspace::new(&root);

        let resolved = ws.resolve("linked.txt").unwrap();
        ws.write_contained(&resolved, b"rewritten").await.unwrap();

        assert_eq!(std::fs::read_to_string(root.join("linked.txt")).unwrap(), "rewritten");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "host");
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(root.join("linked.txt")).unwrap().permissions().mode();
            assert_eq!(mode & 0o7777, 0o640);
        }
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "no temporary file is left");
    }

    /// A workspace with `notes.md`, an `AGENTS.md` beside it, a `docs/` directory, and a
    /// host directory outside the root.
    #[cfg(any(unix, windows))]
    struct SwapFixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        outside: PathBuf,
    }

    #[cfg(any(unix, windows))]
    fn swap_fixture() -> SwapFixture {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("notes.md"), "notes").unwrap();
        std::fs::write(root.join("docs/guide.md"), "guide").unwrap();
        std::fs::write(root.join("AGENTS.md"), "instructions").unwrap();
        std::fs::write(outside.join("secret.txt"), "host secret").unwrap();
        std::fs::write(outside.join("guide.md"), "host guide").unwrap();
        SwapFixture { _temp: temp, root, outside }
    }

    /// Replaces `path` with a link to `target`, as a concurrent process would; `None`
    /// when [`link`] cannot make one.
    #[cfg(any(unix, windows))]
    fn swap_for_link(path: &Path, target: &Path) -> Option<()> {
        let staged = path.with_extension("swap");
        link(target, &staged)?;
        if path.is_dir() {
            std::fs::rename(path, path.with_extension("moved")).unwrap();
        }
        std::fs::rename(&staged, path).unwrap();
        Some(())
    }

    /// Makes `at` a link to `target`: a symlink on Unix, and on Windows a junction to a
    /// directory or a symlink to anything else.
    ///
    /// `None` when Windows refuses the symlink because creating one needs Developer Mode
    /// or an elevated shell; the caller then skips its test. CI always has the privilege,
    /// so there the refusal fails the test instead.
    #[cfg(any(unix, windows))]
    fn link(target: &Path, at: &Path) -> Option<()> {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, at).unwrap();
        #[cfg(windows)]
        if target.is_dir() {
            junction(target, at);
        } else {
            return privileged(std::os::windows::fs::symlink_file(target, at));
        }
        Some(())
    }

    /// Makes `at` a junction (a mount point) to the directory `target`, which needs no
    /// privilege.
    #[cfg(windows)]
    fn junction(target: &Path, at: &Path) {
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(at)
            .arg(target)
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "mklink /J failed: {}",
            String::from_utf8_lossy(&made.stdout)
        );
    }

    /// `Some` when a Windows symlink was made, `None` when the process lacks the privilege
    /// outside CI.
    #[cfg(windows)]
    fn privileged(made: std::io::Result<()>) -> Option<()> {
        let not_held = windows_sys::Win32::Foundation::ERROR_PRIVILEGE_NOT_HELD as i32;
        match made {
            Ok(()) => Some(()),
            Err(error)
                if error.raw_os_error() == Some(not_held) && std::env::var_os("CI").is_none() =>
            {
                eprintln!("skipped: creating a symlink needs Developer Mode or an elevated shell");
                None
            }
            Err(error) => panic!("could not create a symlink: {error}"),
        }
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn a_file_swapped_for_a_link_inside_the_root_after_the_check_is_not_written() {
        // The harness decides on the target a path resolves to; a link swapped in
        // afterwards must not carry the write to a protected file.
        let f = swap_fixture();
        let ws = Workspace::new(&f.root);
        let (_, target) = ws.resolve_target("notes.md").unwrap();
        let Some(()) = swap_for_link(&f.root.join("notes.md"), &f.root.join("AGENTS.md")) else {
            return;
        };

        let result = ws.write_contained(&target, b"rewritten").await;

        assert!(matches!(result, Err(DevToolError::PathEscape(_))), "{result:?}");
        assert_eq!(std::fs::read_to_string(f.root.join("AGENTS.md")).unwrap(), "instructions");
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn a_file_swapped_for_a_link_outside_the_root_after_the_check_is_not_read() {
        let f = swap_fixture();
        let ws = Workspace::new(&f.root);
        let (_, target) = ws.resolve_target("notes.md").unwrap();
        let Some(()) = swap_for_link(&f.root.join("notes.md"), &f.outside.join("secret.txt"))
        else {
            return;
        };

        let result = ws.read_contained(&target).await;

        assert!(matches!(result, Err(DevToolError::PathEscape(_))), "{result:?}");
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn a_directory_swapped_for_a_link_after_the_check_is_not_followed() {
        let f = swap_fixture();
        let ws = Workspace::new(&f.root);
        let (_, read_target) = ws.resolve_target("docs/guide.md").unwrap();
        let (_, write_target) = ws.resolve_target("docs/new.md").unwrap();
        swap_for_link(&f.root.join("docs"), &f.outside).unwrap();

        let read = ws.read_contained(&read_target).await;
        let write = ws.write_contained(&write_target, b"planted").await;

        assert!(matches!(read, Err(DevToolError::PathEscape(_))), "{read:?}");
        assert!(matches!(write, Err(DevToolError::PathEscape(_))), "{write:?}");
        assert!(!f.outside.join("new.md").exists(), "the write landed outside the root");
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn a_missing_directory_planted_as_a_link_after_the_check_is_not_followed() {
        let f = swap_fixture();
        let ws = Workspace::new(&f.root);
        let (_, target) = ws.resolve_target("build/out/report.md").unwrap();
        link(&f.outside, &f.root.join("build")).unwrap();

        let result = ws.write_contained(&target, b"planted").await;

        assert!(matches!(result, Err(DevToolError::PathEscape(_))), "{result:?}");
        assert!(!f.outside.join("out").exists(), "a directory was created outside the root");
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn the_checked_target_is_read_and_written_through_an_inside_link() {
        let f = swap_fixture();
        let Some(()) = link(&f.root.join("notes.md"), &f.root.join("alias.md")) else { return };
        let ws = Workspace::new(&f.root);
        let (lexical, target) = ws.resolve_target("alias.md").unwrap();
        assert_eq!(lexical, ws.root().join("alias.md"));
        assert_eq!(target, ws.root().join("notes.md"));

        assert_eq!(ws.read_contained(&target).await.unwrap(), "notes");
        ws.write_contained(&target, b"updated").await.unwrap();
        assert_eq!(std::fs::read_to_string(f.root.join("notes.md")).unwrap(), "updated");
        let (_, created) = ws.resolve_target("new/dir/file.md").unwrap();
        ws.write_contained(&created, b"made").await.unwrap();
        assert_eq!(std::fs::read_to_string(f.root.join("new/dir/file.md")).unwrap(), "made");
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn a_file_with_more_than_one_hard_link_is_not_read() {
        let f = swap_fixture();
        std::fs::hard_link(f.outside.join("secret.txt"), f.root.join("linked.txt")).unwrap();
        let ws = Workspace::new(&f.root);
        let (_, target) = ws.resolve_target("linked.txt").unwrap();

        let result = ws.read_contained(&target).await;

        let Err(DevToolError::Other(message)) = result else {
            panic!("a hard-linked file was read: {result:?}");
        };
        assert!(message.contains("2 hard links"), "{message}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_directory_swapped_for_a_directory_symlink_after_the_check_is_not_followed() {
        // `link` makes a junction for a directory; a directory symlink carries the other
        // name-surrogate tag.
        let f = swap_fixture();
        let ws = Workspace::new(&f.root);
        let (_, read_target) = ws.resolve_target("docs/guide.md").unwrap();
        let (_, write_target) = ws.resolve_target("docs/new.md").unwrap();
        let docs = f.root.join("docs");
        let staged = docs.with_extension("swap");
        let Some(()) = privileged(std::os::windows::fs::symlink_dir(&f.outside, &staged)) else {
            return;
        };
        std::fs::rename(&docs, docs.with_extension("moved")).unwrap();
        std::fs::rename(&staged, &docs).unwrap();

        let read = ws.read_contained(&read_target).await;
        let write = ws.write_contained(&write_target, b"planted").await;

        assert!(matches!(read, Err(DevToolError::PathEscape(_))), "{read:?}");
        assert!(matches!(write, Err(DevToolError::PathEscape(_))), "{write:?}");
        assert!(!f.outside.join("new.md").exists(), "the write landed outside the root");
    }

    /// A WOF-compressed file is backed by `IO_REPARSE_TAG_WOF`, an in-place tag like a
    /// cloud placeholder's. A real placeholder needs a cloud sync provider registered and
    /// connected through the Cloud Filter API, which a unit test cannot stand up. WOF hides
    /// the reparse point from the handle, so the read and write take the ordinary path.
    #[cfg(windows)]
    #[tokio::test]
    async fn an_in_place_reparse_point_is_read_and_written() {
        use std::os::windows::ffi::OsStrExt as _;
        use windows_sys::Win32::Storage::FileSystem::{WOF_PROVIDER_FILE, WofIsExternalFile};

        let f = swap_fixture();
        let packed = f.root.join("packed.txt");
        let text = "an in-place reparse point keeps its data with its filter\n".repeat(32_768);
        std::fs::write(&packed, &text).unwrap();
        let compact = std::process::Command::new("compact.exe")
            .args(["/C", "/EXE:XPRESS4K"])
            .arg(&packed)
            .output()
            .unwrap();
        // Whether the file is WOF-backed is asked of WOF itself.
        let wide: Vec<u16> = packed.as_os_str().encode_wide().chain(Some(0)).collect();
        let (mut external, mut provider) = (0, 0);
        // SAFETY: `wide` is NUL-terminated and the out-pointers are writable; no file
        // information is requested.
        let asked = unsafe {
            WofIsExternalFile(
                wide.as_ptr(),
                &raw mut external,
                &raw mut provider,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert!(
            asked >= 0 && external != 0 && provider == WOF_PROVIDER_FILE,
            "compact did not back the file with WOF: {}",
            String::from_utf8_lossy(&compact.stdout)
        );
        let ws = Workspace::new(&f.root);
        let (_, target) = ws.resolve_target("packed.txt").unwrap();

        assert_eq!(ws.read_contained(&target).await.unwrap(), text);
        ws.write_contained(&target, b"rewritten").await.unwrap();

        assert_eq!(std::fs::read_to_string(&packed).unwrap(), "rewritten");
    }

    /// Sets a reparse point on the file `path` whose tag no filter on the volume serves: a
    /// third-party tag, neither a link nor a junction, whose file keeps its own data.
    /// Returns the tag.
    #[cfg(windows)]
    fn set_unserved_reparse_point(path: &Path) -> u32 {
        use std::os::windows::fs::OpenOptionsExt as _;
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        use windows_sys::Win32::System::IO::DeviceIoControl;
        use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;

        // Bit 31 clear: not a Microsoft tag, so its buffer carries a GUID. Bit 29 clear: not
        // a name surrogate.
        const TAG: u32 = 0x0000_7ADC;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap();
        // REPARSE_GUID_DATA_BUFFER: the tag, the data's length, two reserved bytes, the
        // owner's GUID, then the data.
        let data = b"adk-devtools test placeholder";
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&TAG.to_le_bytes());
        buffer.extend_from_slice(&(data.len() as u16).to_le_bytes());
        buffer.extend_from_slice(&[0, 0]);
        buffer.extend_from_slice(&[0x41; 16]);
        buffer.extend_from_slice(data);
        let mut returned = 0u32;
        // SAFETY: the handle is open; the input buffer is readable for its length; there is
        // no output buffer, and `returned` is writable.
        let set = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_SET_REPARSE_POINT,
                buffer.as_ptr().cast(),
                buffer.len() as u32,
                std::ptr::null_mut(),
                0,
                &raw mut returned,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(set, 0, "FSCTL_SET_REPARSE_POINT failed: {}", std::io::Error::last_os_error());
        TAG
    }

    /// The reparse tag a handle opened on `path` itself reports, `None` for no reparse point.
    #[cfg(windows)]
    fn visible_reparse_tag(path: &Path) -> Option<u32> {
        use std::os::windows::fs::OpenOptionsExt as _;
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_OPEN_REPARSE_POINT,
            FileAttributeTagInfo, GetFileInformationByHandleEx,
        };

        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap();
        let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
        // SAFETY: `tag` is writable and the size passed is its own; the handle is open.
        let queried = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileAttributeTagInfo,
                (&raw mut tag).cast(),
                size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
            )
        };
        assert_ne!(queried, 0, "{}", std::io::Error::last_os_error());
        (tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0).then_some(tag.ReparseTag)
    }

    /// An in-place reparse point the handle shows — neither a link nor a junction — passes
    /// the open's checks, and the file tools read and write its content through a reopen of
    /// the handle without `FILE_FLAG_OPEN_REPARSE_POINT`, which the tag's filter serves (spec
    /// 3.2 §10 W6). WOF hides its tag from the handle, so the test above takes the ordinary
    /// path; a third-party tag stays visible. No filter serves it, so the reopen fails: the
    /// read is refused and the write leaves the content in place rather than truncate it.
    #[cfg(windows)]
    #[tokio::test]
    async fn an_unserved_in_place_reparse_point_fails_closed_at_the_reopen() {
        use std::io::Read as _;
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Foundation::ERROR_STOPPED_ON_SYMLINK;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

        let f = swap_fixture();
        let path = f.root.join("placeholder.txt");
        std::fs::write(&path, "placeholder content").unwrap();
        let tag = set_unserved_reparse_point(&path);
        assert_eq!(visible_reparse_tag(&path), Some(tag), "the handle shows the tag");
        assert!(!crate::beneath::is_name_surrogate(tag));
        let ws = Workspace::new(&f.root);
        // `resolve_target` canonicalizes the path, which opens through the point and fails;
        // the tools open the canonical target it returns for an ordinary file.
        let target = ws.root().join("placeholder.txt");

        let read = ws.read_contained(&target).await;
        let write = ws.write_contained(&target, b"rewritten").await;

        // An `Io` error other than the link refusal: the open passed its checks, and the
        // reopen through the unserved tag failed.
        let stopped = Some(ERROR_STOPPED_ON_SYMLINK as i32);
        assert!(
            matches!(&read, Err(DevToolError::Io(error)) if error.raw_os_error() != stopped),
            "{read:?}"
        );
        assert!(
            matches!(&write, Err(DevToolError::Io(error)) if error.raw_os_error() != stopped),
            "{write:?}"
        );
        let mut content = String::new();
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&path)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "placeholder content", "the refused write truncated the file");
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
