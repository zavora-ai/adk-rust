//! Digest-bearing read records (spec 3.1 §7.26, §10 D7).
//!
//! A read records the sha256 of what was read, and a write by the agent the sha256 of what
//! it wrote. `edit_file` without `expect_sha256` uses that digest as its precondition, so an
//! edit of a file that changed since it was read writes nothing. The records sit behind
//! [`ReadLedger`] so a host persists them with the session: a held edit approved from a new
//! process still finds the read that preceded it.

use std::collections::HashMap;
use std::sync::Mutex;

use adk_core::{Result, ToolContext};
use async_trait::async_trait;
use sha2::{Digest as _, Sha256};

/// How many (session, agent) scopes [`InMemoryReadLedger`] keeps before evicting the least
/// recently used one.
pub(crate) const READ_SCOPE_CAPACITY: usize = 1024;

/// The session and agent a read is recorded for (spec 3.1 §10 D7).
///
/// Session ids are unique only within an app and user, so both are part of the key.
///
/// # Example
///
/// ```rust
/// use adk_devtools::ReadScope;
///
/// let scope = ReadScope::new("app", "alice", "s1", "coder");
/// assert_eq!(scope.agent, "coder");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ReadScope {
    /// The application.
    pub app: String,
    /// The user the session belongs to.
    pub user: String,
    /// The session.
    pub session: String,
    /// The agent that read the file.
    pub agent: String,
}

impl ReadScope {
    /// A scope from its four parts.
    pub fn new(
        app: impl Into<String>,
        user: impl Into<String>,
        session: impl Into<String>,
        agent: impl Into<String>,
    ) -> Self {
        Self { app: app.into(), user: user.into(), session: session.into(), agent: agent.into() }
    }

    /// The scope of the call behind `ctx`, or `None` when it names no session.
    pub fn of(ctx: &dyn ToolContext) -> Option<Self> {
        let session = ctx.session_id();
        if session.is_empty() {
            return None;
        }
        Some(Self::new(ctx.app_name(), ctx.user_id(), session, ctx.agent_name()))
    }
}

/// Where read records live (spec 3.1 §10 D7).
///
/// `path` is the file's path relative to the workspace root with `/` separators, and
/// `sha256` its lowercase hex content digest. [`InMemoryReadLedger`] is the default; a host
/// that resumes sessions in another process persists the records, as adk-harness does in
/// its store.
///
/// # Example
///
/// ```rust
/// use adk_devtools::{InMemoryReadLedger, ReadLedger, ReadScope};
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let ledger = InMemoryReadLedger::new();
/// let scope = ReadScope::new("app", "alice", "s1", "coder");
/// ledger.record(&scope, "src/lib.rs", "ab12").await?;
/// assert_eq!(ledger.digest(&scope, "src/lib.rs").await?.as_deref(), Some("ab12"));
/// ledger.forget(&scope, "src/lib.rs").await?;
/// assert_eq!(ledger.digest(&scope, "src/lib.rs").await?, None);
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
#[async_trait]
pub trait ReadLedger: Send + Sync {
    /// Records that `scope` read or wrote `path` and saw content with digest `sha256`,
    /// replacing an earlier record.
    ///
    /// # Errors
    ///
    /// Returns the store's error when the record cannot be written.
    async fn record(&self, scope: &ReadScope, path: &str, sha256: &str) -> Result<()>;

    /// The digest `scope` last recorded for `path`.
    ///
    /// # Errors
    ///
    /// Returns the store's error when the record cannot be read.
    async fn digest(&self, scope: &ReadScope, path: &str) -> Result<Option<String>>;

    /// Forgets `path` for `scope`: the file was deleted or moved away.
    ///
    /// # Errors
    ///
    /// Returns the store's error when the record cannot be removed.
    async fn forget(&self, scope: &ReadScope, path: &str) -> Result<()>;
}

/// Read records in process memory, per scope, with least-recently-used eviction of scopes
/// (spec 3.1 §10 D2, D7).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_devtools::{InMemoryReadLedger, Workspace};
///
/// let ws = Workspace::new("./my-repo").with_read_ledger(Arc::new(InMemoryReadLedger::new()));
/// # let _ = ws;
/// ```
#[derive(Default)]
pub struct InMemoryReadLedger {
    inner: Mutex<Scopes>,
}

#[derive(Default)]
struct Scopes {
    /// Records per scope, with the tick of the scope's last use.
    scoped: HashMap<ReadScope, (u64, HashMap<String, String>)>,
    tick: u64,
}

impl Scopes {
    /// The records of `scope`, created (evicting the least recently used scope at
    /// capacity) if absent.
    fn files(&mut self, scope: &ReadScope) -> &mut HashMap<String, String> {
        self.tick += 1;
        if !self.scoped.contains_key(scope)
            && self.scoped.len() >= READ_SCOPE_CAPACITY
            && let Some(oldest) = self
                .scoped
                .iter()
                .min_by_key(|(_, (used, _))| *used)
                .map(|(oldest, _)| oldest.clone())
        {
            self.scoped.remove(&oldest);
        }
        let entry = self.scoped.entry(scope.clone()).or_default();
        entry.0 = self.tick;
        &mut entry.1
    }

    /// The records of `scope` when it has any, marking it used.
    fn existing(&mut self, scope: &ReadScope) -> Option<&mut HashMap<String, String>> {
        self.tick += 1;
        let tick = self.tick;
        self.scoped.get_mut(scope).map(|entry| {
            entry.0 = tick;
            &mut entry.1
        })
    }
}

impl InMemoryReadLedger {
    /// An empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(crate) fn scopes(&self) -> usize {
        self.inner.lock().map(|inner| inner.scoped.len()).unwrap_or_default()
    }
}

#[async_trait]
impl ReadLedger for InMemoryReadLedger {
    async fn record(&self, scope: &ReadScope, path: &str, sha256: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.files(scope).insert(path.to_string(), sha256.to_string());
        Ok(())
    }

    async fn digest(&self, scope: &ReadScope, path: &str) -> Result<Option<String>> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        Ok(inner.existing(scope).and_then(|files| files.get(path).cloned()))
    }

    async fn forget(&self, scope: &ReadScope, path: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(files) = inner.existing(scope) {
            files.remove(path);
        }
        Ok(())
    }
}

/// The lowercase hex sha256 of `bytes`, as read records, `expect_sha256` and the
/// `apply_patch` report write it.
///
/// # Example
///
/// ```rust
/// assert_eq!(
///     adk_devtools::sha256_hex(b""),
///     "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
/// );
/// ```
pub fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(out, "{byte:02x}");
    }
    out
}
