//! `FactStore`: the system of record of long-term facts (spec 3.1 §7.15.2).

use std::collections::HashMap;
use std::path::PathBuf;

use adk_core::principal::SubjectRef;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

use super::types::{FactFilter, FactScope, FactSource, FactStatus, MemoryFact};

/// The system of record of long-term facts (spec 3.1 §7.15.2).
///
/// The store keeps every fact with its provenance and history; a [`FactIndex`] only finds
/// candidates, so status, scope and forgetting are enforced by reading the store
/// (spec 3.1 §9.2 I44). Implementations: [`InMemoryFactStore`], [`FileFactStore`] and,
/// with feature `sqlite`, `SqliteFactStore`.
///
/// | Method | Contract |
/// |---|---|
/// | `put` | stores a fact; a live fact with the same (scope, digest) is kept instead, its `uses` bumped, and returned |
/// | `set_status` | changes a live fact's status; `Superseded` and `Forgotten` set `valid_to`; promoting `Assumed` to `Known` records the source `UserConfirmed`; a fact that is no longer live keeps its status |
/// | `embeddings` | the vectors of the live facts of exactly `scope` |
/// | `record_recall` | sets `last_recalled` and bumps `uses` of recalled facts |
///
/// [`FactIndex`]: crate::FactIndex
///
/// # Example
///
/// ```rust
/// use adk_harness::{FactKind, FactScope, FactSource, FactStatus, FactStore, InMemoryFactStore, MemoryFact, Provenance};
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let store = InMemoryFactStore::new();
/// let scope = FactScope::new("app", "alice", None);
/// let fact = MemoryFact::new(scope, FactKind::Preference, "Use pnpm, not npm", FactStatus::Known,
///     Provenance::new(FactSource::UserStated, "s1", "inv-1"));
/// let stored = store.put(fact.clone()).await.unwrap();
/// let again = store.put(MemoryFact { id: "other".into(), ..fact }).await.unwrap();
/// assert_eq!((again.id, again.uses), (stored.id, 1));
/// # });
/// ```
#[async_trait]
pub trait FactStore: Send + Sync {
    /// Stores `fact`, or bumps and returns the live fact with the same (scope, digest).
    async fn put(&self, fact: MemoryFact) -> Result<MemoryFact>;
    /// The fact with `id`.
    async fn get(&self, id: &str) -> Result<Option<MemoryFact>>;
    /// The facts `filter` matches, newest first.
    async fn list(&self, filter: FactFilter) -> Result<Vec<MemoryFact>>;
    /// Changes the status of the live fact `id`; `by` is the person or service doing it.
    async fn set_status(&self, id: &str, status: FactStatus, by: &SubjectRef) -> Result<()>;
    /// Stores the embedding of fact `id`.
    async fn put_embedding(&self, id: &str, vector: Vec<f32>) -> Result<()>;
    /// The embeddings of the live facts of exactly `scope`.
    async fn embeddings(&self, scope: &FactScope) -> Result<Vec<(String, Vec<f32>)>>;
    /// Records that `ids` were recalled at `at`: `last_recalled` is set and `uses` bumped.
    /// The default does nothing.
    async fn record_recall(&self, ids: &[String], at: DateTime<Utc>) -> Result<()> {
        let _ = (ids, at);
        Ok(())
    }
}

pub(crate) fn not_found(id: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::NotFound,
        "harness.memory.fact_not_found",
        format!("there is no fact '{id}'; /memory lists the facts"),
    )
}

fn not_live(fact: &MemoryFact) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Conflict,
        "harness.memory.fact_not_live",
        format!(
            "fact '{}' is {} and keeps that status; record the fact again instead",
            fact.id,
            fact.status.label()
        ),
    )
}

/// Applies a status change to `fact` under the store contract.
pub(crate) fn apply_status(fact: &mut MemoryFact, status: FactStatus) -> Result<()> {
    if !fact.status.is_live() {
        return Err(not_live(fact));
    }
    if status == FactStatus::Known
        && fact.status == FactStatus::Assumed
        && fact.provenance.source != FactSource::UserStated
    {
        fact.provenance.source = FactSource::UserConfirmed;
    }
    if !status.is_live() {
        fact.valid_to = Some(Utc::now());
    }
    fact.status = status;
    Ok(())
}

fn same_live(a: &MemoryFact, b: &MemoryFact) -> bool {
    a.status.is_live() && a.scope == b.scope && a.digest == b.digest
}

fn sort_newest_first(facts: &mut [MemoryFact]) {
    facts.sort_by(|a, b| b.valid_from.cmp(&a.valid_from).then_with(|| b.id.cmp(&a.id)));
}

#[derive(Default, Serialize, Deserialize)]
struct Facts {
    #[serde(default)]
    facts: HashMap<String, MemoryFact>,
    #[serde(default)]
    embeddings: HashMap<String, Vec<f32>>,
}

impl Facts {
    fn put(&mut self, fact: MemoryFact) -> MemoryFact {
        if let Some(existing) = self.facts.values_mut().find(|existing| same_live(existing, &fact))
        {
            existing.uses = existing.uses.saturating_add(1);
            return existing.clone();
        }
        self.facts.insert(fact.id.clone(), fact.clone());
        fact
    }

    fn list(&self, filter: &FactFilter) -> Vec<MemoryFact> {
        let mut facts: Vec<MemoryFact> =
            self.facts.values().filter(|fact| filter.matches(fact)).cloned().collect();
        sort_newest_first(&mut facts);
        facts
    }

    fn set_status(&mut self, id: &str, status: FactStatus) -> Result<()> {
        let fact = self.facts.get_mut(id).ok_or_else(|| not_found(id))?;
        apply_status(fact, status)
    }

    fn put_embedding(&mut self, id: &str, vector: Vec<f32>) -> Result<()> {
        if !self.facts.contains_key(id) {
            return Err(not_found(id));
        }
        self.embeddings.insert(id.to_string(), vector);
        Ok(())
    }

    fn embeddings(&self, scope: &FactScope) -> Vec<(String, Vec<f32>)> {
        let mut out: Vec<(String, Vec<f32>)> = self
            .embeddings
            .iter()
            .filter(|(id, _)| {
                self.facts
                    .get(*id)
                    .is_some_and(|fact| fact.status.is_live() && fact.scope == *scope)
            })
            .map(|(id, vector)| (id.clone(), vector.clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn record_recall(&mut self, ids: &[String], at: DateTime<Utc>) {
        for id in ids {
            if let Some(fact) = self.facts.get_mut(id) {
                fact.last_recalled = Some(at);
                fact.uses = fact.uses.saturating_add(1);
            }
        }
    }
}

/// A [`FactStore`] in process memory, the builder default (spec 3.1 §7.2).
///
/// # Example
///
/// ```rust
/// let store = adk_harness::InMemoryFactStore::new();
/// # let _ = store;
/// ```
#[derive(Default)]
pub struct InMemoryFactStore {
    facts: RwLock<Facts>,
}

impl std::fmt::Debug for InMemoryFactStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemoryFactStore").finish_non_exhaustive()
    }
}

impl InMemoryFactStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl FactStore for InMemoryFactStore {
    async fn put(&self, fact: MemoryFact) -> Result<MemoryFact> {
        Ok(self.facts.write().await.put(fact))
    }

    async fn get(&self, id: &str) -> Result<Option<MemoryFact>> {
        Ok(self.facts.read().await.facts.get(id).cloned())
    }

    async fn list(&self, filter: FactFilter) -> Result<Vec<MemoryFact>> {
        Ok(self.facts.read().await.list(&filter))
    }

    async fn set_status(&self, id: &str, status: FactStatus, _by: &SubjectRef) -> Result<()> {
        self.facts.write().await.set_status(id, status)
    }

    async fn put_embedding(&self, id: &str, vector: Vec<f32>) -> Result<()> {
        self.facts.write().await.put_embedding(id, vector)
    }

    async fn embeddings(&self, scope: &FactScope) -> Result<Vec<(String, Vec<f32>)>> {
        Ok(self.facts.read().await.embeddings(scope))
    }

    async fn record_recall(&self, ids: &[String], at: DateTime<Utc>) -> Result<()> {
        self.facts.write().await.record_recall(ids, at);
        Ok(())
    }
}

/// A [`FactStore`] of one private JSON file per user (spec 3.1 §7.15.2).
///
/// Files are `<root>/<sha256(app, user)>.json`, written to a temporary sibling and renamed
/// into place with mode `0600` on Unix, so no identifier reaches a path. Writers in one
/// process take turns; the default root is `~/.adk/harness/facts`.
///
/// # Example
///
/// ```rust
/// use adk_harness::{FactKind, FactScope, FactSource, FactStatus, FactStore, FileFactStore, MemoryFact, Provenance};
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let dir = tempfile::tempdir().unwrap();
/// let fact = MemoryFact::new(FactScope::new("app", "alice", None), FactKind::Preference,
///     "Use pnpm", FactStatus::Assumed, Provenance::new(FactSource::AgentInferred, "s", "i"));
/// FileFactStore::new(dir.path()).put(fact.clone()).await.unwrap();
/// assert!(FileFactStore::new(dir.path()).get(&fact.id).await.unwrap().is_some());
/// # });
/// ```
#[derive(Debug)]
pub struct FileFactStore {
    root: PathBuf,
    lock: Mutex<()>,
}

impl FileFactStore {
    /// A store under `root`, created on first write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), lock: Mutex::new(()) }
    }

    /// A store under `~/.adk/harness/facts`.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the home directory cannot be determined.
    pub fn in_home() -> Result<Self> {
        Ok(Self::new(crate::fsutil::adk_home()?.join("harness").join("facts")))
    }

    fn path(&self, app: &str, user: &str) -> PathBuf {
        self.root.join(format!("{}.json", crate::fsutil::sha256_hex(&format!("{app}\u{1f}{user}"))))
    }

    async fn load(path: &std::path::Path) -> Result<Facts> {
        match crate::fsutil::read_optional(path).await? {
            Some(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                AdkError::new(
                    ErrorComponent::Agent,
                    ErrorCategory::Internal,
                    "harness.memory.store_corrupt",
                    format!("{} is not a fact file: {error}", path.display()),
                )
            }),
            None => Ok(Facts::default()),
        }
    }

    async fn save(path: &std::path::Path, facts: &Facts) -> Result<()> {
        let bytes = serde_json::to_vec(facts)
            .map_err(|error| crate::fsutil::serialize_error("facts", &error))?;
        crate::fsutil::write_atomic_private(path, &bytes).await
    }

    /// The files of every user.
    async fn all_files(&self) -> Result<Vec<PathBuf>> {
        let mut entries = match tokio::fs::read_dir(&self.root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(AdkError::new(
                    ErrorComponent::Agent,
                    ErrorCategory::Unavailable,
                    "harness.store.io",
                    format!("read {}: {error}", self.root.display()),
                ));
            }
        };
        let mut files = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                files.push(path);
            }
        }
        Ok(files)
    }

    /// The file holding fact `id`, with its contents.
    async fn find(&self, id: &str) -> Result<Option<(PathBuf, Facts)>> {
        for path in self.all_files().await? {
            let facts = Self::load(&path).await?;
            if facts.facts.contains_key(id) {
                return Ok(Some((path, facts)));
            }
        }
        Ok(None)
    }
}

#[async_trait]
impl FactStore for FileFactStore {
    async fn put(&self, fact: MemoryFact) -> Result<MemoryFact> {
        let _guard = self.lock.lock().await;
        let path = self.path(&fact.scope.app, &fact.scope.user);
        let mut facts = Self::load(&path).await?;
        let stored = facts.put(fact);
        Self::save(&path, &facts).await?;
        Ok(stored)
    }

    async fn get(&self, id: &str) -> Result<Option<MemoryFact>> {
        Ok(self.find(id).await?.and_then(|(_, facts)| facts.facts.get(id).cloned()))
    }

    async fn list(&self, filter: FactFilter) -> Result<Vec<MemoryFact>> {
        Ok(Self::load(&self.path(&filter.app, &filter.user)).await?.list(&filter))
    }

    async fn set_status(&self, id: &str, status: FactStatus, _by: &SubjectRef) -> Result<()> {
        let _guard = self.lock.lock().await;
        let (path, mut facts) = self.find(id).await?.ok_or_else(|| not_found(id))?;
        facts.set_status(id, status)?;
        Self::save(&path, &facts).await
    }

    async fn put_embedding(&self, id: &str, vector: Vec<f32>) -> Result<()> {
        let _guard = self.lock.lock().await;
        let (path, mut facts) = self.find(id).await?.ok_or_else(|| not_found(id))?;
        facts.put_embedding(id, vector)?;
        Self::save(&path, &facts).await
    }

    async fn embeddings(&self, scope: &FactScope) -> Result<Vec<(String, Vec<f32>)>> {
        Ok(Self::load(&self.path(&scope.app, &scope.user)).await?.embeddings(scope))
    }

    async fn record_recall(&self, ids: &[String], at: DateTime<Utc>) -> Result<()> {
        let _guard = self.lock.lock().await;
        for path in self.all_files().await? {
            let mut facts = Self::load(&path).await?;
            if ids.iter().any(|id| facts.facts.contains_key(id)) {
                facts.record_recall(ids, at);
                Self::save(&path, &facts).await?;
            }
        }
        Ok(())
    }
}

/// A [`FactStore`] in SQLite (spec 3.1 §7.15.2, feature `sqlite`): the CLI keeps it in
/// `harness.db`.
///
/// Call [`migrate`](Self::migrate) once before use. The tables are `adk_harness_facts` and
/// `adk_harness_fact_embeddings`, distinct from every other store's tables.
///
/// # Example
///
/// ```rust,ignore
/// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
/// let store = adk_harness::SqliteFactStore::new(pool);
/// store.migrate().await?;
/// ```
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub struct SqliteFactStore {
    pool: sqlx::SqlitePool,
}

#[cfg(feature = "sqlite")]
impl SqliteFactStore {
    /// A store over `pool`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    /// Creates the store's tables if they do not exist.
    ///
    /// # Errors
    ///
    /// Returns the database error when a statement fails.
    pub async fn migrate(&self) -> Result<()> {
        for statement in [
            "CREATE TABLE IF NOT EXISTS adk_harness_facts (\
                id TEXT PRIMARY KEY, app_name TEXT NOT NULL, user_id TEXT NOT NULL, \
                project TEXT, digest TEXT NOT NULL, live INTEGER NOT NULL, \
                valid_from TEXT NOT NULL, record TEXT NOT NULL)",
            "CREATE INDEX IF NOT EXISTS adk_harness_facts_scope \
                ON adk_harness_facts (app_name, user_id, project, digest)",
            "CREATE TABLE IF NOT EXISTS adk_harness_fact_embeddings (\
                id TEXT PRIMARY KEY, vector TEXT NOT NULL)",
        ] {
            sqlx::query(statement).execute(&self.pool).await.map_err(crate::fsutil::db_error)?;
        }
        Ok(())
    }

    fn decode(text: &str) -> Result<MemoryFact> {
        serde_json::from_str(text).map_err(|error| {
            AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Internal,
                "harness.memory.store_corrupt",
                format!("a stored fact does not parse: {error}"),
            )
        })
    }

    async fn write(
        executor: impl sqlx::Executor<'_, Database = sqlx::Sqlite>,
        fact: &MemoryFact,
    ) -> Result<()> {
        let record = serde_json::to_string(fact)
            .map_err(|error| crate::fsutil::serialize_error("fact", &error))?;
        sqlx::query(
            "INSERT INTO adk_harness_facts (id, app_name, user_id, project, digest, live, valid_from, record) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (id) DO UPDATE SET live = excluded.live, record = excluded.record",
        )
        .bind(&fact.id)
        .bind(&fact.scope.app)
        .bind(&fact.scope.user)
        .bind(&fact.scope.project)
        .bind(&fact.digest)
        .bind(fact.status.is_live())
        .bind(fact.valid_from.to_rfc3339())
        .bind(record)
        .execute(executor)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
#[async_trait]
impl FactStore for SqliteFactStore {
    async fn put(&self, fact: MemoryFact) -> Result<MemoryFact> {
        let mut tx = self.pool.begin().await.map_err(crate::fsutil::db_error)?;
        let existing: Option<(String,)> = sqlx::query_as(
            "SELECT record FROM adk_harness_facts WHERE app_name = ? AND user_id = ? \
             AND project IS ? AND digest = ? AND live = 1 LIMIT 1",
        )
        .bind(&fact.scope.app)
        .bind(&fact.scope.user)
        .bind(&fact.scope.project)
        .bind(&fact.digest)
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::fsutil::db_error)?;
        let stored = match existing {
            Some((record,)) => {
                let mut existing = Self::decode(&record)?;
                existing.uses = existing.uses.saturating_add(1);
                existing
            }
            None => fact,
        };
        Self::write(&mut *tx, &stored).await?;
        tx.commit().await.map_err(crate::fsutil::db_error)?;
        Ok(stored)
    }

    async fn get(&self, id: &str) -> Result<Option<MemoryFact>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT record FROM adk_harness_facts WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(crate::fsutil::db_error)?;
        row.map(|(record,)| Self::decode(&record)).transpose()
    }

    async fn list(&self, filter: FactFilter) -> Result<Vec<MemoryFact>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT record FROM adk_harness_facts WHERE app_name = ? AND user_id = ? \
             AND (? = 0 OR live = 1)",
        )
        .bind(&filter.app)
        .bind(&filter.user)
        .bind(filter.live_only)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        let mut facts = Vec::with_capacity(rows.len());
        for (record,) in rows {
            let fact = Self::decode(&record)?;
            if filter.matches(&fact) {
                facts.push(fact);
            }
        }
        sort_newest_first(&mut facts);
        Ok(facts)
    }

    async fn set_status(&self, id: &str, status: FactStatus, _by: &SubjectRef) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(crate::fsutil::db_error)?;
        let row: Option<(String,)> =
            sqlx::query_as("SELECT record FROM adk_harness_facts WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(crate::fsutil::db_error)?;
        let mut fact = Self::decode(&row.ok_or_else(|| not_found(id))?.0)?;
        apply_status(&mut fact, status)?;
        Self::write(&mut *tx, &fact).await?;
        tx.commit().await.map_err(crate::fsutil::db_error)
    }

    async fn put_embedding(&self, id: &str, vector: Vec<f32>) -> Result<()> {
        if self.get(id).await?.is_none() {
            return Err(not_found(id));
        }
        let vector = serde_json::to_string(&vector)
            .map_err(|error| crate::fsutil::serialize_error("embedding", &error))?;
        sqlx::query(
            "INSERT INTO adk_harness_fact_embeddings (id, vector) VALUES (?, ?) \
             ON CONFLICT (id) DO UPDATE SET vector = excluded.vector",
        )
        .bind(id)
        .bind(vector)
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }

    async fn embeddings(&self, scope: &FactScope) -> Result<Vec<(String, Vec<f32>)>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT e.id, e.vector FROM adk_harness_fact_embeddings e \
             JOIN adk_harness_facts f ON f.id = e.id \
             WHERE f.app_name = ? AND f.user_id = ? AND f.project IS ? AND f.live = 1 \
             ORDER BY e.id",
        )
        .bind(&scope.app)
        .bind(&scope.user)
        .bind(&scope.project)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, vector)| {
                serde_json::from_str::<Vec<f32>>(&vector).ok().map(|vector| (id, vector))
            })
            .collect())
    }

    async fn record_recall(&self, ids: &[String], at: DateTime<Utc>) -> Result<()> {
        for id in ids {
            let mut tx = self.pool.begin().await.map_err(crate::fsutil::db_error)?;
            let row: Option<(String,)> =
                sqlx::query_as("SELECT record FROM adk_harness_facts WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(crate::fsutil::db_error)?;
            if let Some((record,)) = row {
                let mut fact = Self::decode(&record)?;
                fact.last_recalled = Some(at);
                fact.uses = fact.uses.saturating_add(1);
                Self::write(&mut *tx, &fact).await?;
            }
            tx.commit().await.map_err(crate::fsutil::db_error)?;
        }
        Ok(())
    }
}
