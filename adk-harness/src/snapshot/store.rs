//! Snapshot manifests (spec 3.1 §7.27): one record per snapshot, in memory or in
//! `harness.db`.

use std::sync::Mutex;

use adk_core::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::walk::NotCovered;

/// Why a snapshot was taken (spec 3.1 §9.3 `harness.snapshot.taken`).
///
/// # Example
///
/// ```rust
/// use adk_harness::snapshot::SnapshotKind;
///
/// assert_eq!(serde_json::to_value(SnapshotKind::Turn).unwrap(), "turn");
/// assert_eq!(SnapshotKind::Rewind.as_str(), "rewind");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotKind {
    /// Before a user turn: the first model call of a run that carries a user message.
    Turn,
    /// Before a plan-graph step.
    Step,
    /// Before a `Destructive` call, such as `git reset --hard` (§7.30).
    Destructive,
    /// Before a rewind, so the rewind itself can be redone.
    Rewind,
}

impl SnapshotKind {
    /// The kind as the audit record names it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Step => "step",
            Self::Destructive => "destructive",
            Self::Rewind => "rewind",
        }
    }
}

impl std::fmt::Display for SnapshotKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One file of a snapshot's manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    /// Relative to the workspace root, with `/` separators.
    pub path: String,
    /// The object id of its content (for a symlink, of its target).
    pub sha256: String,
    /// Length in bytes.
    pub len: u64,
    /// The permission bits on Unix; `0` elsewhere.
    #[serde(default)]
    pub mode: u32,
    /// Modification time in nanoseconds since the Unix epoch, for the stat cache.
    #[serde(default)]
    pub mtime_ns: u64,
    /// The inode number on Unix, for the stat cache.
    #[serde(default)]
    pub inode: u64,
    /// For a symlink, its target; the link is restored as a link, never followed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl FileEntry {
    /// Whether two entries hold the same content, kind and permissions.
    pub fn same_content(&self, other: &Self) -> bool {
        self.sha256 == other.sha256 && self.mode == other.mode && self.link == other.link
    }
}

/// Where the session's plan stood when a snapshot was taken (spec 3.1 §7.11, §7.27).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanMark {
    /// The installed plan's hash.
    pub plan_hash: String,
    /// The graph thread turns ran on.
    pub thread_id: String,
    /// The steps completed on that thread, in plan order.
    pub completed: Vec<String>,
    /// Whether the plan was still running.
    pub running: bool,
}

/// The events a rewind removed from the session, kept with the snapshot taken before it so
/// `/redo` puts them back (spec 3.1 §7.27).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptTail {
    /// The id of the last event the rewind kept; `None` when it removed every event.
    pub after: Option<String>,
    /// The removed events, serialized.
    pub events: Vec<Value>,
}

/// One snapshot's record (spec 3.1 §7.27): who and what it is for, and the object holding
/// its file list. Stored sealed under the store key when one is configured.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRecord {
    /// `snap-` and a random id.
    pub id: String,
    /// The application.
    pub app_name: String,
    /// The user.
    pub user_id: String,
    /// The session.
    pub session_id: String,
    /// The workspace: [`root_digest`](super::root_digest) of its canonical root.
    pub root: String,
    /// When the walk started.
    pub created_at: DateTime<Utc>,
    /// Why it was taken.
    pub kind: SnapshotKind,
    /// The run it was taken in.
    pub invocation_id: String,
    /// The plan step it precedes (kind `step`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// The object holding the file list ([`FileEntry`] array, JSON).
    pub files_object: String,
    /// How many files it holds.
    pub file_count: usize,
    /// Their total length.
    pub total_bytes: u64,
    /// Bytes this snapshot added to the object store.
    pub bytes_new: u64,
    /// Whether the walk stopped at `max_files`.
    pub partial: bool,
    /// Files the walk saw and left out, at most [`NOT_COVERED_LISTED`](super::NOT_COVERED_LISTED).
    #[serde(default)]
    pub not_covered: Vec<NotCovered>,
    /// How many more were left out than listed.
    #[serde(default)]
    pub not_covered_more: usize,
    /// Where the plan stood.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanMark>,
    /// For kind `rewind`: the events the rewind removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<TranscriptTail>,
    /// For kind `rewind`: what the rewind then wrote, path to hash (`None` removed), so its
    /// own writes are never a conflict for the next rewind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewound: Option<std::collections::BTreeMap<String, Option<String>>>,
}

/// Where snapshot records are kept (spec 3.1 §7.27): [`InMemorySnapshotStore`], or
/// `SqliteSnapshotStore` in `harness.db` (feature `sqlite`).
///
/// Lists are oldest first.
#[async_trait]
pub trait SnapshotStore: Send + Sync {
    /// Stores `record`, replacing one with the same id.
    async fn put(&self, record: &SnapshotRecord) -> Result<()>;

    /// The record `id`.
    async fn get(&self, id: &str) -> Result<Option<SnapshotRecord>>;

    /// The records of a session, oldest first.
    async fn list_session(
        &self,
        app: &str,
        user: &str,
        session: &str,
    ) -> Result<Vec<SnapshotRecord>>;

    /// The records of a workspace (by root digest), oldest first.
    async fn list_root(&self, root: &str) -> Result<Vec<SnapshotRecord>>;

    /// Removes the records `ids`.
    async fn delete(&self, ids: &[String]) -> Result<()>;
}

fn oldest_first(records: &mut [SnapshotRecord]) {
    records.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
}

/// Snapshot records in memory.
///
/// # Example
///
/// ```rust
/// use adk_harness::snapshot::{InMemorySnapshotStore, SnapshotStore};
///
/// # tokio_test_block(async {
/// let store = InMemorySnapshotStore::new();
/// assert!(store.list_session("app", "alice", "s1").await.unwrap().is_empty());
/// # });
/// # fn tokio_test_block(f: impl std::future::Future<Output = ()>) {
/// #     tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
/// # }
/// ```
#[derive(Debug, Default)]
pub struct InMemorySnapshotStore {
    records: Mutex<Vec<SnapshotRecord>>,
}

impl InMemorySnapshotStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SnapshotStore for InMemorySnapshotStore {
    async fn put(&self, record: &SnapshotRecord) -> Result<()> {
        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        records.retain(|known| known.id != record.id);
        records.push(record.clone());
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<SnapshotRecord>> {
        Ok(self
            .records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|record| record.id == id)
            .cloned())
    }

    async fn list_session(
        &self,
        app: &str,
        user: &str,
        session: &str,
    ) -> Result<Vec<SnapshotRecord>> {
        let mut out: Vec<SnapshotRecord> = self
            .records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|r| r.app_name == app && r.user_id == user && r.session_id == session)
            .cloned()
            .collect();
        oldest_first(&mut out);
        Ok(out)
    }

    async fn list_root(&self, root: &str) -> Result<Vec<SnapshotRecord>> {
        let mut out: Vec<SnapshotRecord> = self
            .records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|record| record.root == root)
            .cloned()
            .collect();
        oldest_first(&mut out);
        Ok(out)
    }

    async fn delete(&self, ids: &[String]) -> Result<()> {
        self.records.lock().unwrap_or_else(|e| e.into_inner()).retain(|r| !ids.contains(&r.id));
        Ok(())
    }
}

/// Snapshot records in a SQLite database, `harness.db` in adk-cli: table
/// `adk_harness_snapshots`, the record sealed under the store key when one is configured
/// (spec 3.1 §7.12, I68).
///
/// # Example
///
/// ```rust,ignore
/// let store = adk_harness::snapshot::SqliteSnapshotStore::new(pool);
/// store.migrate().await?;
/// ```
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub struct SqliteSnapshotStore {
    pool: sqlx::SqlitePool,
    #[cfg(feature = "encrypted")]
    envelope: Option<crate::encrypted::StoreEnvelope>,
}

#[cfg(feature = "sqlite")]
impl SqliteSnapshotStore {
    /// A store over `pool`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self {
            pool,
            #[cfg(feature = "encrypted")]
            envelope: None,
        }
    }

    /// Seals every record under `keyring` (spec 3.1 §7.12).
    #[cfg(feature = "encrypted")]
    #[must_use]
    pub fn with_keyring(mut self, keyring: crate::encrypted::StoreKeyring) -> Self {
        self.envelope = Some(crate::encrypted::StoreEnvelope::new(keyring, "snapshots"));
        self
    }

    /// Re-seals every record a previous key sealed, seals records written before a key was
    /// configured, and returns how many rows changed. Without a keyring, nothing changes.
    ///
    /// # Errors
    ///
    /// Returns `harness.store.decrypt_failed` when a row opens under no key the keyring
    /// holds, or the database error.
    #[cfg(feature = "encrypted")]
    pub async fn rewrap(&self) -> Result<u64> {
        let Some(envelope) = &self.envelope else {
            return Ok(0);
        };
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT id, record FROM adk_harness_snapshots")
                .fetch_all(&self.pool)
                .await
                .map_err(crate::fsutil::db_error)?;
        let mut changed = 0;
        for (id, text) in rows {
            if let Some(resealed) = envelope.reseal_or_seal(&[&id], &text)? {
                sqlx::query("UPDATE adk_harness_snapshots SET record = ? WHERE id = ?")
                    .bind(resealed)
                    .bind(&id)
                    .execute(&self.pool)
                    .await
                    .map_err(crate::fsutil::db_error)?;
                changed += 1;
            }
        }
        Ok(changed)
    }

    /// Creates the store's table if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns the database error when a statement fails.
    pub async fn migrate(&self) -> Result<()> {
        for statement in [
            "CREATE TABLE IF NOT EXISTS adk_harness_snapshots (\
                id TEXT PRIMARY KEY, app_name TEXT NOT NULL, user_id TEXT NOT NULL, \
                session_id TEXT NOT NULL, root TEXT NOT NULL, created_at TEXT NOT NULL, \
                kind TEXT NOT NULL, record TEXT NOT NULL)",
            "CREATE INDEX IF NOT EXISTS adk_harness_snapshots_session \
                ON adk_harness_snapshots (app_name, user_id, session_id)",
            "CREATE INDEX IF NOT EXISTS adk_harness_snapshots_root \
                ON adk_harness_snapshots (root)",
        ] {
            sqlx::query(statement).execute(&self.pool).await.map_err(crate::fsutil::db_error)?;
        }
        Ok(())
    }

    #[cfg(feature = "encrypted")]
    fn encode(&self, id: &str, text: String) -> Result<String> {
        crate::encrypted::encode_row(self.envelope.as_ref(), &[id], text)
    }

    #[cfg(not(feature = "encrypted"))]
    fn encode(&self, _id: &str, text: String) -> Result<String> {
        Ok(text)
    }

    #[cfg(feature = "encrypted")]
    fn decode(&self, id: &str, text: String) -> Result<String> {
        crate::encrypted::decode_row(self.envelope.as_ref(), "snapshots", &[id], text)
    }

    #[cfg(not(feature = "encrypted"))]
    fn decode(&self, _id: &str, text: String) -> Result<String> {
        Ok(text)
    }

    fn parse(&self, id: String, text: String) -> Result<Option<SnapshotRecord>> {
        let text = self.decode(&id, text)?;
        // A record that does not parse restores nothing.
        Ok(serde_json::from_str(&text).ok())
    }

    async fn select(&self, sql: &'static str, binds: &[&str]) -> Result<Vec<SnapshotRecord>> {
        let mut query = sqlx::query_as::<_, (String, String)>(sql);
        for bind in binds {
            query = query.bind(*bind);
        }
        let rows = query.fetch_all(&self.pool).await.map_err(crate::fsutil::db_error)?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, text) in rows {
            if let Some(record) = self.parse(id, text)? {
                out.push(record);
            }
        }
        oldest_first(&mut out);
        Ok(out)
    }
}

#[cfg(feature = "sqlite")]
#[async_trait]
impl SnapshotStore for SqliteSnapshotStore {
    async fn put(&self, record: &SnapshotRecord) -> Result<()> {
        let text = serde_json::to_string(record)
            .map_err(|error| crate::fsutil::serialize_error("snapshot record", &error))?;
        let text = self.encode(&record.id, text)?;
        sqlx::query(
            "INSERT INTO adk_harness_snapshots \
             (id, app_name, user_id, session_id, root, created_at, kind, record) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (id) DO UPDATE SET record = excluded.record",
        )
        .bind(&record.id)
        .bind(&record.app_name)
        .bind(&record.user_id)
        .bind(&record.session_id)
        .bind(&record.root)
        .bind(record.created_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
        .bind(record.kind.as_str())
        .bind(text)
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<SnapshotRecord>> {
        Ok(self
            .select("SELECT id, record FROM adk_harness_snapshots WHERE id = ?", &[id])
            .await?
            .into_iter()
            .next())
    }

    async fn list_session(
        &self,
        app: &str,
        user: &str,
        session: &str,
    ) -> Result<Vec<SnapshotRecord>> {
        self.select(
            "SELECT id, record FROM adk_harness_snapshots \
             WHERE app_name = ? AND user_id = ? AND session_id = ?",
            &[app, user, session],
        )
        .await
    }

    async fn list_root(&self, root: &str) -> Result<Vec<SnapshotRecord>> {
        self.select("SELECT id, record FROM adk_harness_snapshots WHERE root = ?", &[root]).await
    }

    async fn delete(&self, ids: &[String]) -> Result<()> {
        for id in ids {
            sqlx::query("DELETE FROM adk_harness_snapshots WHERE id = ?")
                .bind(id)
                .execute(&self.pool)
                .await
                .map_err(crate::fsutil::db_error)?;
        }
        Ok(())
    }
}
