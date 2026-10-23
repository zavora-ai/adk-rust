//! `FactStore`: the system of record of long-term facts (spec 3.1 §7.15.2).

use std::collections::HashMap;
use std::path::PathBuf;

use adk_core::principal::SubjectRef;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

use super::consolidate::ConsolidationRecord;
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
/// | `distilled_sessions`, `mark_distilled` | the cursor of the sessions distillation read, which only moves forward (spec 3.2 §10 HK2) |
/// | `put_consolidation`, `consolidations` | the records of consolidation ticks, newest first (spec 3.2 §7.5) |
/// | `restore_assumed` | sets a superseded or forgotten fact back to `Assumed`, clearing `valid_to`: how `undo` reverts a tick |
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

    /// The sessions of `user` of `app` that distillation has read, each with the time of
    /// the last event it read (spec 3.2 §10 HK2): the cursor that keeps a consolidation tick
    /// from distilling a session the per-session distiller already handled. The default
    /// keeps no cursor.
    async fn distilled_sessions(
        &self,
        app: &str,
        user: &str,
    ) -> Result<Vec<(String, DateTime<Utc>)>> {
        let _ = (app, user);
        Ok(Vec::new())
    }

    /// Records that distillation read session `session` of `user` of `app` up to `upto`;
    /// an earlier `upto` than the recorded one changes nothing (spec 3.2 §10 HK2). The
    /// default does nothing.
    async fn mark_distilled(
        &self,
        app: &str,
        user: &str,
        session: &str,
        upto: DateTime<Utc>,
    ) -> Result<()> {
        let _ = (app, user, session, upto);
        Ok(())
    }

    /// Stores, or replaces, the record of consolidation tick `record.tick_id`
    /// (spec 3.2 §7.5).
    ///
    /// # Errors
    ///
    /// The default returns `harness.memory.consolidation_unsupported`: a tick runs only
    /// on a store that keeps its record, so `undo` can revert it.
    async fn put_consolidation(&self, record: &ConsolidationRecord) -> Result<()> {
        let _ = record;
        Err(consolidation_unsupported())
    }

    /// The consolidation records of `user` of `app`, newest first (spec 3.2 §7.5). The
    /// default has none.
    async fn consolidations(&self, app: &str, user: &str) -> Result<Vec<ConsolidationRecord>> {
        let _ = (app, user);
        Ok(Vec::new())
    }

    /// Sets fact `id`, superseded or forgotten, back to `Assumed` and clears `valid_to`
    /// (spec 3.2 §7.5): how `undo` reverts a consolidation tick, which changes only
    /// `Assumed` facts. `by` is the person or service doing it.
    ///
    /// # Errors
    ///
    /// Returns `harness.memory.fact_not_found`, `harness.memory.fact_live` for a fact that
    /// is live, or, by default, `harness.memory.consolidation_unsupported`.
    async fn restore_assumed(&self, id: &str, by: &SubjectRef) -> Result<()> {
        let _ = (id, by);
        Err(consolidation_unsupported())
    }
}

fn consolidation_unsupported() -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Unsupported,
        "harness.memory.consolidation_unsupported",
        "this fact store keeps no consolidation records, so a tick could not be undone; use \
         InMemoryFactStore, FileFactStore or SqliteFactStore, or implement \
         put_consolidation, consolidations and restore_assumed",
    )
}

fn still_live(fact: &MemoryFact) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Conflict,
        "harness.memory.fact_live",
        format!(
            "fact '{}' is {}; only a superseded or forgotten fact is restored",
            fact.id,
            fact.status.label()
        ),
    )
}

/// Sets a superseded or forgotten `fact` back to `Assumed`.
pub(crate) fn apply_restore(fact: &mut MemoryFact) -> Result<()> {
    if fact.status.is_live() {
        return Err(still_live(fact));
    }
    fact.status = FactStatus::Assumed;
    fact.valid_to = None;
    Ok(())
}

/// The key of a session's distillation cursor in the in-memory and file stores.
fn cursor_key(app: &str, user: &str, session: &str) -> String {
    format!("{app}\u{1f}{user}\u{1f}{session}")
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
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    cursors: HashMap<String, DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    ticks: HashMap<String, ConsolidationRecord>,
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

    fn distilled_sessions(&self, app: &str, user: &str) -> Vec<(String, DateTime<Utc>)> {
        let prefix = cursor_key(app, user, "");
        let mut out: Vec<(String, DateTime<Utc>)> = self
            .cursors
            .iter()
            .filter_map(|(key, upto)| Some((key.strip_prefix(&prefix)?.to_string(), *upto)))
            .collect();
        out.sort();
        out
    }

    fn mark_distilled(&mut self, app: &str, user: &str, session: &str, upto: DateTime<Utc>) {
        let cursor = self.cursors.entry(cursor_key(app, user, session)).or_insert(upto);
        *cursor = (*cursor).max(upto);
    }

    fn consolidations(&self, app: &str, user: &str) -> Vec<ConsolidationRecord> {
        let mut out: Vec<ConsolidationRecord> = self
            .ticks
            .values()
            .filter(|record| record.app == app && record.user == user)
            .cloned()
            .collect();
        out.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.tick_id.cmp(&a.tick_id)));
        out
    }

    fn restore_assumed(&mut self, id: &str) -> Result<()> {
        apply_restore(self.facts.get_mut(id).ok_or_else(|| not_found(id))?)
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

    async fn distilled_sessions(
        &self,
        app: &str,
        user: &str,
    ) -> Result<Vec<(String, DateTime<Utc>)>> {
        Ok(self.facts.read().await.distilled_sessions(app, user))
    }

    async fn mark_distilled(
        &self,
        app: &str,
        user: &str,
        session: &str,
        upto: DateTime<Utc>,
    ) -> Result<()> {
        self.facts.write().await.mark_distilled(app, user, session, upto);
        Ok(())
    }

    async fn put_consolidation(&self, record: &ConsolidationRecord) -> Result<()> {
        self.facts.write().await.ticks.insert(record.tick_id.clone(), record.clone());
        Ok(())
    }

    async fn consolidations(&self, app: &str, user: &str) -> Result<Vec<ConsolidationRecord>> {
        Ok(self.facts.read().await.consolidations(app, user))
    }

    async fn restore_assumed(&self, id: &str, _by: &SubjectRef) -> Result<()> {
        self.facts.write().await.restore_assumed(id)
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

    async fn distilled_sessions(
        &self,
        app: &str,
        user: &str,
    ) -> Result<Vec<(String, DateTime<Utc>)>> {
        Ok(Self::load(&self.path(app, user)).await?.distilled_sessions(app, user))
    }

    async fn mark_distilled(
        &self,
        app: &str,
        user: &str,
        session: &str,
        upto: DateTime<Utc>,
    ) -> Result<()> {
        let _guard = self.lock.lock().await;
        let path = self.path(app, user);
        let mut facts = Self::load(&path).await?;
        facts.mark_distilled(app, user, session, upto);
        Self::save(&path, &facts).await
    }

    async fn put_consolidation(&self, record: &ConsolidationRecord) -> Result<()> {
        let _guard = self.lock.lock().await;
        let path = self.path(&record.app, &record.user);
        let mut facts = Self::load(&path).await?;
        facts.ticks.insert(record.tick_id.clone(), record.clone());
        Self::save(&path, &facts).await
    }

    async fn consolidations(&self, app: &str, user: &str) -> Result<Vec<ConsolidationRecord>> {
        Ok(Self::load(&self.path(app, user)).await?.consolidations(app, user))
    }

    async fn restore_assumed(&self, id: &str, _by: &SubjectRef) -> Result<()> {
        let _guard = self.lock.lock().await;
        let (path, mut facts) = self.find(id).await?.ok_or_else(|| not_found(id))?;
        facts.restore_assumed(id)?;
        Self::save(&path, &facts).await
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
    #[cfg(feature = "encrypted")]
    envelope: Option<crate::encrypted::StoreEnvelope>,
}

#[cfg(feature = "sqlite")]
impl SqliteFactStore {
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
        self.envelope = Some(crate::encrypted::StoreEnvelope::new(keyring, "facts"));
        self
    }

    /// Re-seals every record a previous key sealed with the current key, seals records
    /// written before a key was configured, and returns how many rows changed (spec 3.1
    /// §7.12). Without a keyring, nothing changes.
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
            sqlx::query_as("SELECT id, record FROM adk_harness_facts")
                .fetch_all(&self.pool)
                .await
                .map_err(crate::fsutil::db_error)?;
        let mut changed = 0;
        for (id, text) in rows {
            if let Some(resealed) = envelope.reseal_or_seal(&[&id], &text)? {
                sqlx::query("UPDATE adk_harness_facts SET record = ? WHERE id = ?")
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

    #[cfg(feature = "encrypted")]
    fn encode(&self, address: &[&str], text: String) -> Result<String> {
        crate::encrypted::encode_row(self.envelope.as_ref(), address, text)
    }

    #[cfg(not(feature = "encrypted"))]
    fn encode(&self, _address: &[&str], text: String) -> Result<String> {
        Ok(text)
    }

    #[cfg(feature = "encrypted")]
    fn decode(&self, address: &[&str], text: String) -> Result<String> {
        crate::encrypted::decode_row(self.envelope.as_ref(), "facts", address, text)
    }

    #[cfg(not(feature = "encrypted"))]
    fn decode(&self, _address: &[&str], text: String) -> Result<String> {
        Ok(text)
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
            "CREATE TABLE IF NOT EXISTS adk_harness_fact_cursors (\
                app_name TEXT NOT NULL, user_id TEXT NOT NULL, session_id TEXT NOT NULL, \
                upto TEXT NOT NULL, PRIMARY KEY (app_name, user_id, session_id))",
            "CREATE TABLE IF NOT EXISTS adk_harness_fact_ticks (\
                tick_id TEXT PRIMARY KEY, app_name TEXT NOT NULL, user_id TEXT NOT NULL, \
                at TEXT NOT NULL, record TEXT NOT NULL)",
        ] {
            sqlx::query(statement).execute(&self.pool).await.map_err(crate::fsutil::db_error)?;
        }
        Ok(())
    }

    fn decode_fact(&self, id: &str, text: String) -> Result<MemoryFact> {
        let text = self.decode(&[id], text)?;
        serde_json::from_str(&text).map_err(|error| {
            AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Internal,
                "harness.memory.store_corrupt",
                format!("a stored fact does not parse: {error}"),
            )
        })
    }

    async fn write(
        &self,
        executor: impl sqlx::Executor<'_, Database = sqlx::Sqlite>,
        fact: &MemoryFact,
    ) -> Result<()> {
        let record = serde_json::to_string(fact)
            .map_err(|error| crate::fsutil::serialize_error("fact", &error))?;
        let record = self.encode(&[&fact.id], record)?;
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
        let existing: Option<(String, String)> = sqlx::query_as(
            "SELECT id, record FROM adk_harness_facts WHERE app_name = ? AND user_id = ? \
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
            Some((id, record)) => {
                let mut existing = self.decode_fact(&id, record)?;
                existing.uses = existing.uses.saturating_add(1);
                existing
            }
            None => fact,
        };
        self.write(&mut *tx, &stored).await?;
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
        row.map(|(record,)| self.decode_fact(id, record)).transpose()
    }

    async fn list(&self, filter: FactFilter) -> Result<Vec<MemoryFact>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, record FROM adk_harness_facts WHERE app_name = ? AND user_id = ? \
             AND (? = 0 OR live = 1)",
        )
        .bind(&filter.app)
        .bind(&filter.user)
        .bind(filter.live_only)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        let mut facts = Vec::with_capacity(rows.len());
        for (id, record) in rows {
            let fact = self.decode_fact(&id, record)?;
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
        let mut fact = self.decode_fact(id, row.ok_or_else(|| not_found(id))?.0)?;
        apply_status(&mut fact, status)?;
        self.write(&mut *tx, &fact).await?;
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
                let mut fact = self.decode_fact(id, record)?;
                fact.last_recalled = Some(at);
                fact.uses = fact.uses.saturating_add(1);
                self.write(&mut *tx, &fact).await?;
            }
            tx.commit().await.map_err(crate::fsutil::db_error)?;
        }
        Ok(())
    }

    async fn distilled_sessions(
        &self,
        app: &str,
        user: &str,
    ) -> Result<Vec<(String, DateTime<Utc>)>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT session_id, upto FROM adk_harness_fact_cursors \
             WHERE app_name = ? AND user_id = ? ORDER BY session_id",
        )
        .bind(app)
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(rows
            .into_iter()
            .filter_map(|(session, upto)| {
                let upto = DateTime::parse_from_rfc3339(&upto).ok()?.with_timezone(&Utc);
                Some((session, upto))
            })
            .collect())
    }

    async fn mark_distilled(
        &self,
        app: &str,
        user: &str,
        session: &str,
        upto: DateTime<Utc>,
    ) -> Result<()> {
        // A fixed-width UTC timestamp, so text order is time order.
        let upto = upto.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
        sqlx::query(
            "INSERT INTO adk_harness_fact_cursors (app_name, user_id, session_id, upto) \
             VALUES (?, ?, ?, ?) ON CONFLICT (app_name, user_id, session_id) \
             DO UPDATE SET upto = excluded.upto WHERE excluded.upto > adk_harness_fact_cursors.upto",
        )
        .bind(app)
        .bind(user)
        .bind(session)
        .bind(upto)
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }

    async fn put_consolidation(&self, record: &ConsolidationRecord) -> Result<()> {
        let text = serde_json::to_string(record)
            .map_err(|error| crate::fsutil::serialize_error("consolidation record", &error))?;
        let text = self.encode(&["consolidation", &record.tick_id], text)?;
        sqlx::query(
            "INSERT INTO adk_harness_fact_ticks (tick_id, app_name, user_id, at, record) \
             VALUES (?, ?, ?, ?, ?) ON CONFLICT (tick_id) DO UPDATE SET record = excluded.record",
        )
        .bind(&record.tick_id)
        .bind(&record.app)
        .bind(&record.user)
        .bind(record.at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
        .bind(text)
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }

    async fn consolidations(&self, app: &str, user: &str) -> Result<Vec<ConsolidationRecord>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT tick_id, record FROM adk_harness_fact_ticks \
             WHERE app_name = ? AND user_id = ? ORDER BY at DESC, tick_id DESC",
        )
        .bind(app)
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        let mut out = Vec::with_capacity(rows.len());
        for (tick_id, text) in rows {
            let text = self.decode(&["consolidation", &tick_id], text)?;
            out.push(serde_json::from_str(&text).map_err(|error| {
                AdkError::new(
                    ErrorComponent::Agent,
                    ErrorCategory::Internal,
                    "harness.memory.store_corrupt",
                    format!("a stored consolidation record does not parse: {error}"),
                )
            })?);
        }
        Ok(out)
    }

    async fn restore_assumed(&self, id: &str, _by: &SubjectRef) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(crate::fsutil::db_error)?;
        let row: Option<(String,)> =
            sqlx::query_as("SELECT record FROM adk_harness_facts WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(crate::fsutil::db_error)?;
        let mut fact = self.decode_fact(id, row.ok_or_else(|| not_found(id))?.0)?;
        apply_restore(&mut fact)?;
        self.write(&mut *tx, &fact).await?;
        tx.commit().await.map_err(crate::fsutil::db_error)
    }
}
