//! Checkpointing for persistent graph state.
//!
//! A [`Checkpointer`] stores the [`Checkpoint`] a graph run writes after each
//! super-step, so a run can resume where it stopped, on this process or another.
//!
//! # Contract
//!
//! Every backend — the ones in this crate and any third-party one — keeps these
//! rules, and proves it by passing the conformance suite (the `conformance` feature,
//! `adk_graph::checkpointer_conformance_tests!`):
//!
//! | Rule | Behaviour |
//! |------|-----------|
//! | Ordering | A thread's checkpoints are ordered by [`Checkpoint::version`], never by time. `load` returns the highest version; `list` returns ascending versions |
//! | Monotonic versions | `save` stores a checkpoint only when its version is at least 1 and greater than every version stored for the thread; otherwise it fails with [`GraphError::CheckpointConflict`]. Of concurrent saves of one version, exactly one succeeds |
//! | Unique IDs | A checkpoint ID is stored once; saving a second checkpoint with a stored ID fails |
//! | Thread isolation | `delete` and `prune` touch only the named thread |
//! | Retention | `prune` removes what a [`RetentionPolicy`] selects, and never a checkpoint a resume still needs ([`needed_to_resume`]) or the policy pins |
//! | Fidelity | A loaded checkpoint equals the saved one, with `created_at` to the microsecond |
//!
//! The version rule is what keeps two writers from interleaving. A writer continues a
//! thread from its newest checkpoint, version `n`, and saves `n + 1`; a second writer
//! that read the same checkpoint saves `n + 1` too and is refused.

use crate::error::{GraphError, Result};
use crate::state::Checkpoint;
use async_trait::async_trait;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use tokio::sync::RwLock;

/// How many checkpoints to keep for a thread, and for how long.
///
/// A long-running thread accumulates one checkpoint per super-step. Without a
/// policy that grows without bound, which costs storage and slows a `list`.
///
/// Whatever the limits say, a policy never selects a checkpoint a resume still
/// needs ([`needed_to_resume`]) or one it pins:
///
/// | Kept | Why |
/// |------|-----|
/// | The newest checkpoint | A resume without a checkpoint ID loads it |
/// | Each pause no run has resumed | A caller holds its ID and resumes it with [`ExecutionConfig::with_resume_from`](crate::node::ExecutionConfig::with_resume_from) |
/// | Each effect-log entry | A governed node's call is answered from it instead of running again |
/// | Each ID in [`pinned`](Self::pinned) | The caller asked for it |
///
/// # Example
///
/// ```
/// use adk_graph::checkpoint::RetentionPolicy;
/// use std::time::Duration;
///
/// // Keep the last 50 steps, and nothing older than a week.
/// let policy = RetentionPolicy::keep_last(50).with_max_age(Duration::from_secs(7 * 24 * 3600));
/// # let _ = policy;
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RetentionPolicy {
    /// Keep at most this many checkpoints per thread, newest first.
    pub max_per_thread: Option<usize>,
    /// Remove checkpoints older than this.
    pub max_age: Option<std::time::Duration>,
    /// Checkpoint IDs kept whatever the limits say.
    pub pinned: BTreeSet<String>,
}

impl RetentionPolicy {
    /// Keeps the newest `count` checkpoints for a thread.
    ///
    /// A count of zero is raised to one, because the newest is always kept.
    pub fn keep_last(count: usize) -> Self {
        Self { max_per_thread: Some(count.max(1)), ..Self::default() }
    }

    /// Removes checkpoints older than `age`.
    pub fn max_age(age: std::time::Duration) -> Self {
        Self { max_age: Some(age), ..Self::default() }
    }

    /// Adds an age limit to a count limit.
    pub fn with_max_age(mut self, age: std::time::Duration) -> Self {
        self.max_age = Some(age);
        self
    }

    /// Adds a count limit to an age limit.
    pub fn with_max_per_thread(mut self, count: usize) -> Self {
        self.max_per_thread = Some(count.max(1));
        self
    }

    /// Keeps these checkpoints whatever the limits say.
    ///
    /// A store that hides checkpoint metadata from the store it wraps, as an encrypting
    /// one does, pins what [`needed_to_resume`] finds in its own view before delegating.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::checkpoint::RetentionPolicy;
    ///
    /// let policy = RetentionPolicy::keep_last(1).pinning(["audited-cp"]);
    /// assert!(policy.pinned.contains("audited-cp"));
    /// ```
    pub fn pinning<I, S>(mut self, checkpoint_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.pinned.extend(checkpoint_ids.into_iter().map(Into::into));
        self
    }

    /// Whether this policy would remove anything.
    pub fn is_unlimited(&self) -> bool {
        self.max_per_thread.is_none() && self.max_age.is_none()
    }

    /// Selects the checkpoint IDs this policy discards from one thread's checkpoints.
    ///
    /// Shared by every backend so they cannot disagree about what to keep. Nothing in
    /// [`needed_to_resume`] or [`pinned`](Self::pinned) is selected.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::checkpoint::RetentionPolicy;
    /// use adk_graph::state::{Checkpoint, State};
    ///
    /// let thread: Vec<Checkpoint> = (1..=3)
    ///     .map(|version| Checkpoint::new("t1", State::new(), 0, vec![]).with_version(version))
    ///     .collect();
    /// let expired = RetentionPolicy::keep_last(1).expired(&thread);
    /// assert_eq!(expired, vec![thread[1].checkpoint_id.clone(), thread[0].checkpoint_id.clone()]);
    /// ```
    pub fn expired(&self, checkpoints: &[Checkpoint]) -> Vec<String> {
        if self.is_unlimited() || checkpoints.len() <= 1 {
            return Vec::new();
        }
        let needed = needed_to_resume(checkpoints);
        let mut ordered: Vec<&Checkpoint> = checkpoints.iter().collect();
        // Newest first, so the count limit counts from the one a resume loads.
        ordered.sort_by_key(|checkpoint| std::cmp::Reverse(checkpoint.version));

        let cutoff = self.max_age.and_then(|age| {
            chrono::Duration::from_std(age).ok().map(|age| chrono::Utc::now() - age)
        });

        ordered
            .iter()
            .enumerate()
            .filter(|(index, checkpoint)| {
                !needed.contains(&checkpoint.checkpoint_id)
                    && !self.pinned.contains(&checkpoint.checkpoint_id)
                    && (self.max_per_thread.is_some_and(|max| *index >= max)
                        || cutoff.is_some_and(|cutoff| checkpoint.created_at < cutoff))
            })
            .map(|(_, checkpoint)| checkpoint.checkpoint_id.clone())
            .collect()
    }
}

/// The IDs of one thread's checkpoints that a resume still needs, which no
/// [`RetentionPolicy`] removes.
///
/// | Checkpoint | Needed because |
/// |------------|----------------|
/// | The newest, by version | A resume without a checkpoint ID loads it |
/// | Each one the newest lists in [`OPEN_PAUSES_METADATA_KEY`] | A run paused there and no run has resumed it, so a caller can still resume it by ID |
/// | Each one carrying [`EFFECT_METADATA_KEY`] | An effect-log entry answers a governed node's call, so a fork or a retry does not run it again |
///
/// A checkpoint whose pause has been resumed is not needed: the run that resumed it
/// continues from its own checkpoints. Checkpoints written before the open-pause list
/// existed protect only the newest.
///
/// # Example
///
/// ```
/// use adk_graph::checkpoint::needed_to_resume;
/// use adk_graph::record::OPEN_PAUSES_METADATA_KEY;
/// use adk_graph::state::{Checkpoint, State};
/// use serde_json::json;
///
/// let paused = Checkpoint::new("t1", State::new(), 1, vec!["approve".into()]);
/// let branch = Checkpoint::new("t1", State::new(), 2, vec![])
///     .with_version(2)
///     .with_metadata(OPEN_PAUSES_METADATA_KEY, json!([paused.checkpoint_id]));
/// let needed = needed_to_resume(&[paused.clone(), branch.clone()]);
/// assert!(needed.contains(&paused.checkpoint_id) && needed.contains(&branch.checkpoint_id));
/// ```
///
/// [`OPEN_PAUSES_METADATA_KEY`]: crate::record::OPEN_PAUSES_METADATA_KEY
/// [`EFFECT_METADATA_KEY`]: crate::governed::EFFECT_METADATA_KEY
pub fn needed_to_resume(checkpoints: &[Checkpoint]) -> BTreeSet<String> {
    let mut needed = BTreeSet::new();
    let Some(newest) = checkpoints.iter().max_by_key(|checkpoint| checkpoint.version) else {
        return needed;
    };
    needed.insert(newest.checkpoint_id.clone());
    // A malformed list protects nothing extra rather than blocking every prune; the
    // newest, which a resume loads, is kept regardless.
    needed.extend(newest.open_pauses().unwrap_or_default());
    needed.extend(
        checkpoints
            .iter()
            .filter(|checkpoint| {
                checkpoint.metadata.contains_key(crate::governed::EFFECT_METADATA_KEY)
            })
            .map(|checkpoint| checkpoint.checkpoint_id.clone()),
    );
    needed
}

/// When a graph run writes the checkpoint of each completed super-step.
///
/// | Mode | Checkpoint written | A crash loses |
/// |------|--------------------|---------------|
/// | [`Sync`](Self::Sync) (default) | Before the next super-step starts | Nothing that completed |
/// | [`Async`](Self::Async) | While the next super-step runs | At most the last completed super-step |
/// | [`Exit`](Self::Exit) | When the run completes, pauses, or fails | Everything since the run started |
///
/// Pauses and failures are always written before the run returns, in every mode.
/// A super-step that runs a node declared [`ToolEffect::NonIdempotent`](adk_core::ToolEffect)
/// (see [`CompiledGraph::with_node_effect`](crate::graph::CompiledGraph::with_node_effect))
/// first waits until every earlier checkpoint is written, so a crash during the side
/// effect never resumes from before an earlier step and repeats it. Nodes that declare
/// no effect do not force this.
///
/// # Example
///
/// ```
/// use adk_graph::checkpoint::Durability;
/// use adk_graph::node::ExecutionConfig;
///
/// let config = ExecutionConfig::new("thread").with_durability(Durability::Async);
/// assert_eq!(config.durability, Durability::Async);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Durability {
    /// Writes only when the run exits: completes, pauses, or fails.
    Exit,
    /// Writes each checkpoint in the background while the next super-step runs.
    Async,
    /// Writes each checkpoint before the next super-step starts.
    #[default]
    Sync,
}

/// Checks a checkpoint's version against the highest version stored for its thread.
///
/// Shared by every backend so they refuse the same writes.
///
/// # Example
///
/// ```
/// use adk_graph::checkpoint::ensure_newer;
/// use adk_graph::state::{Checkpoint, State};
///
/// let checkpoint = Checkpoint::new("thread", State::new(), 0, vec![]).with_version(3);
/// assert!(ensure_newer(&checkpoint, Some(2)).is_ok());
/// assert!(ensure_newer(&checkpoint, Some(3)).is_err());
/// ```
///
/// # Errors
///
/// Returns [`GraphError::CheckpointError`] for version 0 and
/// [`GraphError::CheckpointConflict`] when `head` is not below the version.
pub fn ensure_newer(checkpoint: &Checkpoint, head: Option<u64>) -> Result<()> {
    if checkpoint.version == 0 {
        return Err(GraphError::CheckpointError(format!(
            "checkpoint '{}' has version 0; versions start at 1. Set it with \
             Checkpoint::with_version(newest version + 1)",
            checkpoint.checkpoint_id
        )));
    }
    match head {
        Some(head) if head >= checkpoint.version => Err(conflict(checkpoint, head)),
        _ => Ok(()),
    }
}

/// The conflict a refused save reports.
///
/// # Example
///
/// ```
/// use adk_graph::checkpoint::conflict;
/// use adk_graph::state::{Checkpoint, State};
///
/// let checkpoint = Checkpoint::new("thread", State::new(), 0, vec![]);
/// assert!(conflict(&checkpoint, 1).to_string().contains("conflicts"));
/// ```
pub fn conflict(checkpoint: &Checkpoint, head: u64) -> GraphError {
    GraphError::CheckpointConflict {
        thread_id: checkpoint.thread_id.clone(),
        version: checkpoint.version,
        head,
    }
}

/// The error a save reports when the checkpoint ID is already stored.
///
/// # Example
///
/// ```
/// use adk_graph::checkpoint::duplicate_id;
///
/// assert!(duplicate_id("cp-1").to_string().contains("cp-1"));
/// ```
pub fn duplicate_id(checkpoint_id: &str) -> GraphError {
    GraphError::CheckpointError(format!(
        "checkpoint id '{checkpoint_id}' is already stored; every checkpoint needs a unique id"
    ))
}

/// Stores graph checkpoints. See the [module documentation](self) for the contract.
#[async_trait]
pub trait Checkpointer: Send + Sync {
    /// Saves a checkpoint and returns its ID.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::CheckpointConflict`] when the thread already holds an
    /// equal or higher version, and an error when the ID is already stored or the
    /// backend cannot be written.
    async fn save(&self, checkpoint: &Checkpoint) -> Result<String>;

    /// Loads the newest checkpoint of a thread: the one with the highest version.
    async fn load(&self, thread_id: &str) -> Result<Option<Checkpoint>>;

    /// Loads a checkpoint by ID.
    async fn load_by_id(&self, checkpoint_id: &str) -> Result<Option<Checkpoint>>;

    /// Lists a thread's checkpoints, oldest version first.
    async fn list(&self, thread_id: &str) -> Result<Vec<Checkpoint>>;

    /// Lists at most `limit` of a thread's checkpoints, newest version first,
    /// starting below version `before` when given.
    ///
    /// Pass the version of the last checkpoint of one page as `before` to read the
    /// next. The default filters [`list`](Self::list); backends override it to read
    /// one page only.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend cannot be read.
    async fn list_page(
        &self,
        thread_id: &str,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Vec<Checkpoint>> {
        let mut checkpoints = self.list(thread_id).await?;
        checkpoints.retain(|checkpoint| before.is_none_or(|before| checkpoint.version < before));
        checkpoints.sort_by_key(|checkpoint| std::cmp::Reverse(checkpoint.version));
        checkpoints.truncate(limit);
        Ok(checkpoints)
    }

    /// Deletes every checkpoint of a thread, its open pauses included.
    ///
    /// Deleting a thread that holds no checkpoints is a no-op. The thread's versions
    /// start afresh afterwards.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend cannot be written.
    async fn delete(&self, thread_id: &str) -> Result<()>;

    /// Removes the checkpoints a retention policy discards from one thread.
    ///
    /// Backends select with [`RetentionPolicy::expired`], so nothing a resume still
    /// needs ([`needed_to_resume`]) is removed. Returns how many were removed.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend cannot be read or written.
    async fn prune(&self, thread_id: &str, policy: &RetentionPolicy) -> Result<usize>;
}

#[derive(Default)]
struct MemoryStore {
    /// Each thread's checkpoints, ascending by version.
    threads: HashMap<String, Vec<Checkpoint>>,
    /// Checkpoint ID to thread.
    ids: HashMap<String, String>,
}

/// In-memory checkpointer for development and testing.
///
/// # Example
///
/// ```
/// use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
/// use adk_graph::state::{Checkpoint, State};
///
/// # async fn demo() -> adk_graph::Result<()> {
/// let checkpointer = MemoryCheckpointer::new();
/// checkpointer.save(&Checkpoint::new("thread", State::new(), 0, vec![])).await?;
/// assert!(checkpointer.load("thread").await?.is_some());
/// # Ok(())
/// # }
/// ```
#[derive(Default)]
pub struct MemoryCheckpointer {
    store: Arc<RwLock<MemoryStore>>,
}

impl MemoryCheckpointer {
    /// Create a new in-memory checkpointer
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Checkpointer for MemoryCheckpointer {
    async fn save(&self, checkpoint: &Checkpoint) -> Result<String> {
        let mut store = self.store.write().await;
        let head = store
            .threads
            .get(&checkpoint.thread_id)
            .and_then(|thread| thread.last())
            .map(|newest| newest.version);
        ensure_newer(checkpoint, head)?;
        if store.ids.contains_key(&checkpoint.checkpoint_id) {
            return Err(duplicate_id(&checkpoint.checkpoint_id));
        }
        store.ids.insert(checkpoint.checkpoint_id.clone(), checkpoint.thread_id.clone());
        store.threads.entry(checkpoint.thread_id.clone()).or_default().push(checkpoint.clone());
        Ok(checkpoint.checkpoint_id.clone())
    }

    async fn load(&self, thread_id: &str) -> Result<Option<Checkpoint>> {
        let store = self.store.read().await;
        Ok(store.threads.get(thread_id).and_then(|thread| thread.last()).cloned())
    }

    async fn load_by_id(&self, checkpoint_id: &str) -> Result<Option<Checkpoint>> {
        let store = self.store.read().await;
        Ok(store.ids.get(checkpoint_id).and_then(|thread_id| {
            store.threads.get(thread_id)?.iter().find(|c| c.checkpoint_id == checkpoint_id).cloned()
        }))
    }

    async fn list(&self, thread_id: &str) -> Result<Vec<Checkpoint>> {
        let store = self.store.read().await;
        Ok(store.threads.get(thread_id).cloned().unwrap_or_default())
    }

    async fn delete(&self, thread_id: &str) -> Result<()> {
        let mut store = self.store.write().await;
        if let Some(thread) = store.threads.remove(thread_id) {
            for checkpoint in thread {
                store.ids.remove(&checkpoint.checkpoint_id);
            }
        }
        Ok(())
    }

    async fn prune(&self, thread_id: &str, policy: &RetentionPolicy) -> Result<usize> {
        let mut store = self.store.write().await;
        let Some(thread) = store.threads.get_mut(thread_id) else { return Ok(0) };
        let expired = policy.expired(thread);
        if expired.is_empty() {
            return Ok(0);
        }
        let before = thread.len();
        thread.retain(|checkpoint| !expired.contains(&checkpoint.checkpoint_id));
        let removed = before - thread.len();
        for checkpoint_id in &expired {
            store.ids.remove(checkpoint_id);
        }
        Ok(removed)
    }
}

/// SQLite checkpointer.
///
/// Each checkpoint is one row of `graph_checkpoints`. A unique index on
/// `(thread_id, version)` and a guarded insert enforce the version rule, so two
/// processes sharing a database file cannot interleave a thread.
#[cfg(feature = "sqlite")]
pub struct SqliteCheckpointer {
    pool: sqlx::SqlitePool,
}

#[cfg(feature = "sqlite")]
fn sqlite_error(error: sqlx::Error) -> GraphError {
    GraphError::CheckpointError(error.to_string())
}

#[cfg(feature = "sqlite")]
fn sqlite_row(row: &sqlx::sqlite::SqliteRow) -> Result<Checkpoint> {
    use sqlx::Row;
    let text = |column: &str| row.try_get::<String, _>(column).map_err(sqlite_error);
    let optional = |column: &str| row.try_get::<Option<String>, _>(column).map_err(sqlite_error);
    fn json_or_default<T: serde::de::DeserializeOwned + Default>(raw: Option<String>) -> Result<T> {
        Ok(raw.map(|raw| serde_json::from_str(&raw)).transpose()?.unwrap_or_default())
    }
    let attempts = json_or_default(optional("attempts")?)?;
    let child_ledger = json_or_default(optional("child_ledger")?)?;
    let pending_writes = json_or_default(optional("pending_writes")?)?;
    Ok(Checkpoint {
        checkpoint_id: text("id")?,
        thread_id: text("thread_id")?,
        state: serde_json::from_str(&text("state")?)?,
        step: row.try_get::<i64, _>("step").map_err(sqlite_error)? as usize,
        pending_nodes: serde_json::from_str(&text("pending_nodes")?)?,
        metadata: serde_json::from_str(&text("metadata")?)?,
        created_at: chrono::DateTime::parse_from_rfc3339(&text("created_at")?)
            .map_err(|e| GraphError::CheckpointError(e.to_string()))?
            .with_timezone(&chrono::Utc),
        cleared_interrupt: optional("cleared_interrupt")?,
        attempts,
        child_ledger,
        version: row.try_get::<i64, _>("version").map_err(sqlite_error)? as u64,
        pending_writes,
    })
}

#[cfg(feature = "sqlite")]
impl SqliteCheckpointer {
    /// Create a new SQLite checkpointer
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = sqlx::SqlitePool::connect(database_url).await.map_err(sqlite_error)?;
        Self::from_pool(pool).await
    }

    /// Create a SQLite checkpointer from an existing pool.
    ///
    /// Use this to share one connection pool with the rest of an application
    /// instead of opening a second one. The checkpointer writes through the pool
    /// it is given, so the caller's own queries see its rows.
    ///
    /// The schema is applied to the pool's database on every call, so adopting an
    /// already-initialized database is safe. A database written before checkpoints
    /// carried versions has its rows numbered in creation order.
    ///
    /// `SqlitePool` comes from `sqlx`, so a caller has to depend on a
    /// semver-compatible `sqlx` to construct one:
    ///
    /// ```toml
    /// sqlx = { version = "0.9", default-features = false, features = ["runtime-tokio", "sqlite"] }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `GraphError::CheckpointError` when the table, a missing column, or
    /// the index cannot be created on the pool's database.
    pub async fn from_pool(pool: sqlx::SqlitePool) -> Result<Self> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS graph_checkpoints (
                id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL,
                state TEXT NOT NULL,
                step INTEGER NOT NULL,
                pending_nodes TEXT NOT NULL,
                metadata TEXT,
                created_at TEXT NOT NULL,
                cleared_interrupt TEXT,
                attempts TEXT,
                child_ledger TEXT,
                version INTEGER NOT NULL DEFAULT 0,
                pending_writes TEXT
            )
            "#,
        )
        .execute(&pool)
        .await
        .map_err(sqlite_error)?;

        // A database created before a column existed keeps its old shape under
        // CREATE TABLE IF NOT EXISTS, so add each column separately and tolerate only
        // the duplicate-column error on a database that already has it.
        for alter in [
            "ALTER TABLE graph_checkpoints ADD COLUMN cleared_interrupt TEXT",
            "ALTER TABLE graph_checkpoints ADD COLUMN attempts TEXT",
            "ALTER TABLE graph_checkpoints ADD COLUMN child_ledger TEXT",
            "ALTER TABLE graph_checkpoints ADD COLUMN version INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE graph_checkpoints ADD COLUMN pending_writes TEXT",
        ] {
            match sqlx::query(alter).execute(&pool).await {
                Ok(_) => {}
                Err(sqlx::Error::Database(error))
                    if error.message().starts_with("duplicate column name") => {}
                Err(error) => return Err(sqlite_error(error)),
            }
        }

        // Rows written before versions existed are numbered in creation order.
        sqlx::query(
            r#"
            UPDATE graph_checkpoints SET version = (
                SELECT COUNT(*) FROM graph_checkpoints AS earlier
                WHERE earlier.thread_id = graph_checkpoints.thread_id
                  AND (earlier.created_at < graph_checkpoints.created_at
                       OR (earlier.created_at = graph_checkpoints.created_at
                           AND earlier.id <= graph_checkpoints.id))
            )
            WHERE version = 0
            "#,
        )
        .execute(&pool)
        .await
        .map_err(sqlite_error)?;

        for statement in [
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_graph_checkpoints_thread_version \
             ON graph_checkpoints(thread_id, version)",
            "DROP INDEX IF EXISTS idx_graph_checkpoints_thread",
        ] {
            sqlx::query(statement).execute(&pool).await.map_err(sqlite_error)?;
        }

        Ok(Self { pool })
    }

    /// Create an in-memory SQLite checkpointer (for testing)
    pub async fn in_memory() -> Result<Self> {
        Self::new(":memory:").await
    }

    async fn head(&self, thread_id: &str) -> Result<Option<u64>> {
        let head: Option<i64> =
            sqlx::query_scalar("SELECT MAX(version) FROM graph_checkpoints WHERE thread_id = ?")
                .bind(thread_id)
                .fetch_one(&self.pool)
                .await
                .map_err(sqlite_error)?;
        Ok(head.map(|head| head as u64))
    }
}

#[cfg(feature = "sqlite")]
#[async_trait]
impl Checkpointer for SqliteCheckpointer {
    async fn save(&self, checkpoint: &Checkpoint) -> Result<String> {
        ensure_newer(checkpoint, None)?;
        // The guard refuses a stale version in the same statement that inserts, and
        // the unique index refuses a concurrent insert of the same version.
        let inserted = sqlx::query(
            "INSERT INTO graph_checkpoints (id, thread_id, state, step, pending_nodes, metadata, created_at, \
             cleared_interrupt, attempts, child_ledger, version, pending_writes) \
             SELECT ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ? \
             WHERE NOT EXISTS (SELECT 1 FROM graph_checkpoints WHERE thread_id = ? AND version >= ?)",
        )
        .bind(&checkpoint.checkpoint_id)
        .bind(&checkpoint.thread_id)
        .bind(serde_json::to_string(&checkpoint.state)?)
        .bind(checkpoint.step as i64)
        .bind(serde_json::to_string(&checkpoint.pending_nodes)?)
        .bind(serde_json::to_string(&checkpoint.metadata)?)
        .bind(checkpoint.created_at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
        .bind(checkpoint.cleared_interrupt.as_deref())
        .bind(serde_json::to_string(&checkpoint.attempts)?)
        .bind(serde_json::to_string(&checkpoint.child_ledger)?)
        .bind(checkpoint.version as i64)
        .bind(serde_json::to_string(&checkpoint.pending_writes)?)
        .bind(&checkpoint.thread_id)
        .bind(checkpoint.version as i64)
        .execute(&self.pool)
        .await;
        match inserted {
            Ok(result) if result.rows_affected() == 1 => Ok(checkpoint.checkpoint_id.clone()),
            Ok(_) => {
                Err(conflict(checkpoint, self.head(&checkpoint.thread_id).await?.unwrap_or(0)))
            }
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                if error.message().contains("graph_checkpoints.id") {
                    Err(duplicate_id(&checkpoint.checkpoint_id))
                } else {
                    Err(conflict(checkpoint, self.head(&checkpoint.thread_id).await?.unwrap_or(0)))
                }
            }
            Err(error) => Err(sqlite_error(error)),
        }
    }

    async fn load(&self, thread_id: &str) -> Result<Option<Checkpoint>> {
        let row = sqlx::query(
            "SELECT id, thread_id, state, step, pending_nodes, metadata, created_at, \
             cleared_interrupt, attempts, child_ledger, version, pending_writes FROM graph_checkpoints WHERE thread_id = ? \
             ORDER BY version DESC LIMIT 1",
        )
        .bind(thread_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(sqlite_error)?;
        row.as_ref().map(sqlite_row).transpose()
    }

    async fn load_by_id(&self, checkpoint_id: &str) -> Result<Option<Checkpoint>> {
        let row =
            sqlx::query(
            "SELECT id, thread_id, state, step, pending_nodes, metadata, created_at, \
             cleared_interrupt, attempts, child_ledger, version, pending_writes FROM graph_checkpoints WHERE id = ?",
        )
                .bind(checkpoint_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(sqlite_error)?;
        row.as_ref().map(sqlite_row).transpose()
    }

    async fn list(&self, thread_id: &str) -> Result<Vec<Checkpoint>> {
        let rows = sqlx::query(
            "SELECT id, thread_id, state, step, pending_nodes, metadata, created_at, \
             cleared_interrupt, attempts, child_ledger, version, pending_writes FROM graph_checkpoints WHERE thread_id = ? ORDER BY version ASC",
        )
        .bind(thread_id)
        .fetch_all(&self.pool)
        .await
        .map_err(sqlite_error)?;
        rows.iter().map(sqlite_row).collect()
    }

    async fn list_page(
        &self,
        thread_id: &str,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Vec<Checkpoint>> {
        let rows = sqlx::query(
            "SELECT id, thread_id, state, step, pending_nodes, metadata, created_at, \
             cleared_interrupt, attempts, child_ledger, version, pending_writes FROM graph_checkpoints WHERE thread_id = ? AND version < ? \
             ORDER BY version DESC LIMIT ?",
        )
        .bind(thread_id)
        .bind(before.map_or(i64::MAX, |before| before as i64))
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await
        .map_err(sqlite_error)?;
        rows.iter().map(sqlite_row).collect()
    }

    async fn delete(&self, thread_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM graph_checkpoints WHERE thread_id = ?")
            .bind(thread_id)
            .execute(&self.pool)
            .await
            .map_err(sqlite_error)?;
        Ok(())
    }

    async fn prune(&self, thread_id: &str, policy: &RetentionPolicy) -> Result<usize> {
        // The policy decides, not the SQL, so every backend keeps the same set.
        let expired = policy.expired(&self.list(thread_id).await?);
        let mut removed = 0usize;
        for checkpoint_id in &expired {
            let result =
                sqlx::query("DELETE FROM graph_checkpoints WHERE id = ? AND thread_id = ?")
                    .bind(checkpoint_id)
                    .bind(thread_id)
                    .execute(&self.pool)
                    .await
                    .map_err(sqlite_error)?;
            removed += result.rows_affected() as usize;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;

    #[tokio::test]
    async fn test_memory_checkpointer() {
        let cp = MemoryCheckpointer::new();

        // Create and save checkpoint
        let checkpoint = Checkpoint::new("thread_1", State::new(), 0, vec!["node_a".to_string()]);
        let id = cp.save(&checkpoint).await.unwrap();
        assert!(!id.is_empty());

        // Load latest
        let loaded = cp.load("thread_1").await.unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().step, 0);

        // Save another checkpoint
        let checkpoint2 = Checkpoint::new("thread_1", State::new(), 1, vec!["node_b".to_string()])
            .with_version(2);
        cp.save(&checkpoint2).await.unwrap();

        // Load latest should return step 1
        let loaded = cp.load("thread_1").await.unwrap();
        assert_eq!(loaded.unwrap().step, 1);

        // List should return both
        let all = cp.list("thread_1").await.unwrap();
        assert_eq!(all.len(), 2);

        // Delete
        cp.delete("thread_1").await.unwrap();
        let loaded = cp.load("thread_1").await.unwrap();
        assert!(loaded.is_none());
    }
}
