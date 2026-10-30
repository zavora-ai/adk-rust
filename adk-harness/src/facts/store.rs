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

/// A fact's vector and the id of the embedder that produced it (spec 3.1 §7.32, I73).
///
/// Recall compares vectors of one embedder only: a vector whose `embedder` differs from
/// the index's is embedded again, even when the two models share a length. Vectors
/// stored before the id existed read back with an empty `embedder`.
///
/// # Example
///
/// ```rust
/// use adk_harness::FactEmbedding;
///
/// let embedding = FactEmbedding::new("onnx/BAAI/bge-small-en-v1.5", vec![0.6, 0.8]);
/// assert!(embedding.is_from("onnx/BAAI/bge-small-en-v1.5"));
/// assert!(!embedding.is_from("gemini/gemini-embedding-2"));
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FactEmbedding {
    /// The embedder's id: its [`adk_memory::EmbeddingProvider::model_id`], or
    /// `unnamed/<dimensions>` for an embedder without one.
    pub embedder: String,
    /// The vector.
    pub vector: Vec<f32>,
}

impl FactEmbedding {
    /// `vector`, produced by the embedder `embedder`.
    pub fn new(embedder: impl Into<String>, vector: Vec<f32>) -> Self {
        Self { embedder: embedder.into(), vector }
    }

    /// Whether the embedder `embedder` produced this vector, and it is usable.
    pub fn is_from(&self, embedder: &str) -> bool {
        !self.vector.is_empty() && self.embedder == embedder
    }
}

/// A stored vector: tagged, or a bare vector written before embedder ids existed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredEmbedding {
    Tagged(FactEmbedding),
    Untagged(Vec<f32>),
}

impl From<StoredEmbedding> for FactEmbedding {
    fn from(stored: StoredEmbedding) -> Self {
        match stored {
            StoredEmbedding::Tagged(embedding) => embedding,
            StoredEmbedding::Untagged(vector) => Self::new("", vector),
        }
    }
}

/// The system of record of long-term facts (spec 3.1 §7.15.2).
///
/// The store keeps every fact with its provenance and history; a [`FactIndex`] only finds
/// candidates, so status, scope and forgetting are enforced by reading the store
/// (spec 3.1 §9.2 I44). Implementations: [`InMemoryFactStore`], [`FileFactStore`], with
/// feature `sqlite` `SqliteFactStore`, and with feature `postgres` `PostgresFactStore`, the
/// shared store of `adk-rust up --store postgres://…` (spec 3.3 §12 phase 3).
///
/// | Method | Contract |
/// |---|---|
/// | `put` | stores a fact; a live fact with the same (scope, digest) is kept instead, its `uses` bumped, and returned |
/// | `set_status` | changes a live fact's status; `Superseded` and `Forgotten` set `valid_to`; promoting `Assumed` to `Known` records the source `UserConfirmed`; a fact that is no longer live keeps its status |
/// | `put_embedding`, `embeddings` | one vector per fact, tagged with the id of the embedder that produced it; `embeddings` returns those of the live facts of exactly `scope` (spec 3.1 §7.32, I73) |
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
    /// Stores the embedding of fact `id`, replacing an earlier one.
    async fn put_embedding(&self, id: &str, embedding: FactEmbedding) -> Result<()>;
    /// The embeddings of the live facts of exactly `scope`, each with its embedder's id.
    async fn embeddings(&self, scope: &FactScope) -> Result<Vec<(String, FactEmbedding)>>;
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
         InMemoryFactStore, FileFactStore, SqliteFactStore or PostgresFactStore, or implement \
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
    embeddings: HashMap<String, StoredEmbedding>,
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

    fn put_embedding(&mut self, id: &str, embedding: FactEmbedding) -> Result<()> {
        if !self.facts.contains_key(id) {
            return Err(not_found(id));
        }
        self.embeddings.insert(id.to_string(), StoredEmbedding::Tagged(embedding));
        Ok(())
    }

    fn embeddings(&self, scope: &FactScope) -> Vec<(String, FactEmbedding)> {
        let mut out: Vec<(String, FactEmbedding)> = self
            .embeddings
            .iter()
            .filter(|(id, _)| {
                self.facts
                    .get(*id)
                    .is_some_and(|fact| fact.status.is_live() && fact.scope == *scope)
            })
            .map(|(id, stored)| (id.clone(), stored.clone().into()))
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

    async fn put_embedding(&self, id: &str, embedding: FactEmbedding) -> Result<()> {
        self.facts.write().await.put_embedding(id, embedding)
    }

    async fn embeddings(&self, scope: &FactScope) -> Result<Vec<(String, FactEmbedding)>> {
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

    async fn put_embedding(&self, id: &str, embedding: FactEmbedding) -> Result<()> {
        let _guard = self.lock.lock().await;
        let (path, mut facts) = self.find(id).await?.ok_or_else(|| not_found(id))?;
        facts.put_embedding(id, embedding)?;
        Self::save(&path, &facts).await
    }

    async fn embeddings(&self, scope: &FactScope) -> Result<Vec<(String, FactEmbedding)>> {
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
/// `adk_harness_fact_embeddings`, distinct from every other store's tables. A change that
/// reads a fact and writes it back opens with `BEGIN IMMEDIATE`, so it waits for another
/// writer on the file under the pool's busy timeout instead of failing with `database is
/// locked`.
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

/// Opens every transaction that reads a fact and writes it back. A deferred transaction that
/// has read cannot wait for the write lock: SQLite fails its first write at once, whatever
/// the busy timeout.
#[cfg(feature = "sqlite")]
const BEGIN_WRITE: &str = "BEGIN IMMEDIATE";

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
                id TEXT PRIMARY KEY, vector TEXT NOT NULL, embedder TEXT NOT NULL DEFAULT '')",
            "CREATE TABLE IF NOT EXISTS adk_harness_fact_cursors (\
                app_name TEXT NOT NULL, user_id TEXT NOT NULL, session_id TEXT NOT NULL, \
                upto TEXT NOT NULL, PRIMARY KEY (app_name, user_id, session_id))",
            "CREATE TABLE IF NOT EXISTS adk_harness_fact_ticks (\
                tick_id TEXT PRIMARY KEY, app_name TEXT NOT NULL, user_id TEXT NOT NULL, \
                at TEXT NOT NULL, record TEXT NOT NULL)",
        ] {
            sqlx::query(statement).execute(&self.pool).await.map_err(crate::fsutil::db_error)?;
        }
        // Tables created before embedder ids (spec 3.1 §7.32) gain the column; their
        // vectors read back untagged and are embedded again when recall needs them.
        let (tagged,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM pragma_table_info('adk_harness_fact_embeddings') \
             WHERE name = 'embedder'",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        if tagged == 0 {
            sqlx::query(
                "ALTER TABLE adk_harness_fact_embeddings \
                 ADD COLUMN embedder TEXT NOT NULL DEFAULT ''",
            )
            .execute(&self.pool)
            .await
            .map_err(crate::fsutil::db_error)?;
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
        let mut tx = self.pool.begin_with(BEGIN_WRITE).await.map_err(crate::fsutil::db_error)?;
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
        let mut tx = self.pool.begin_with(BEGIN_WRITE).await.map_err(crate::fsutil::db_error)?;
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

    async fn put_embedding(&self, id: &str, embedding: FactEmbedding) -> Result<()> {
        if self.get(id).await?.is_none() {
            return Err(not_found(id));
        }
        let vector = serde_json::to_string(&embedding.vector)
            .map_err(|error| crate::fsutil::serialize_error("embedding", &error))?;
        sqlx::query(
            "INSERT INTO adk_harness_fact_embeddings (id, vector, embedder) VALUES (?, ?, ?) \
             ON CONFLICT (id) DO UPDATE SET vector = excluded.vector, \
             embedder = excluded.embedder",
        )
        .bind(id)
        .bind(vector)
        .bind(&embedding.embedder)
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }

    async fn embeddings(&self, scope: &FactScope) -> Result<Vec<(String, FactEmbedding)>> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT e.id, e.vector, e.embedder FROM adk_harness_fact_embeddings e \
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
            .filter_map(|(id, vector, embedder)| {
                serde_json::from_str::<Vec<f32>>(&vector)
                    .ok()
                    .map(|vector| (id, FactEmbedding::new(embedder, vector)))
            })
            .collect())
    }

    async fn record_recall(&self, ids: &[String], at: DateTime<Utc>) -> Result<()> {
        for id in ids {
            let mut tx =
                self.pool.begin_with(BEGIN_WRITE).await.map_err(crate::fsutil::db_error)?;
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
        let mut tx = self.pool.begin_with(BEGIN_WRITE).await.map_err(crate::fsutil::db_error)?;
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

/// Migrations of [`PostgresFactStore`], recorded in `_adk_harness_fact_migrations`.
#[cfg(feature = "postgres")]
const POSTGRES_FACT_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create long-term fact tables",
    "CREATE TABLE IF NOT EXISTS adk_harness_facts (\
        id TEXT PRIMARY KEY, app_name TEXT NOT NULL, user_id TEXT NOT NULL, \
        project TEXT, digest TEXT NOT NULL, live BOOLEAN NOT NULL, \
        valid_from TEXT NOT NULL, record TEXT NOT NULL); \
     CREATE INDEX IF NOT EXISTS adk_harness_facts_scope \
        ON adk_harness_facts (app_name, user_id, project, digest); \
     CREATE TABLE IF NOT EXISTS adk_harness_fact_embeddings (\
        id TEXT PRIMARY KEY, vector TEXT NOT NULL, embedder TEXT NOT NULL DEFAULT ''); \
     CREATE TABLE IF NOT EXISTS adk_harness_fact_cursors (\
        app_name TEXT NOT NULL, user_id TEXT NOT NULL, session_id TEXT NOT NULL, \
        upto TEXT NOT NULL, PRIMARY KEY (app_name, user_id, session_id)); \
     CREATE TABLE IF NOT EXISTS adk_harness_fact_ticks (\
        tick_id TEXT PRIMARY KEY, app_name TEXT NOT NULL, user_id TEXT NOT NULL, \
        at TEXT NOT NULL, record TEXT NOT NULL);",
)];

/// A fixed-width UTC timestamp, so text order is time order.
#[cfg(feature = "postgres")]
fn sortable(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

#[cfg(feature = "postgres")]
fn corrupt_record(what: &str, error: &serde_json::Error) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Internal,
        "harness.memory.store_corrupt",
        format!("a stored {what} does not parse: {error}"),
    )
}

/// A [`FactStore`] in PostgreSQL, shared by every replica of a served harness and by
/// `adk-rust up --store postgres://…` (spec 3.1 §7.15.2, §13 Q2; spec 3.3 §12 phase 3;
/// feature `postgres`).
///
/// The tables are the SQLite store's, by the same names: `adk_harness_facts`,
/// `adk_harness_fact_embeddings`, `adk_harness_fact_cursors` and `adk_harness_fact_ticks`.
/// [`put`](FactStore::put) takes a transaction-scoped advisory lock on the fact's (scope,
/// digest), so two replicas storing the same fact keep one; every other change reads its
/// row `FOR UPDATE`. [`migrate`](Self::migrate) records its steps in
/// `_adk_harness_fact_migrations` under an advisory lock, so replicas starting together
/// migrate one at a time. Records are not sealed: encrypt the shared database at rest, as
/// for the mode and standing-rule stores.
///
/// # Example
///
/// ```rust,no_run
/// use adk_harness::{FactKind, FactScope, FactSource, FactStatus, FactStore, MemoryFact};
/// use adk_harness::{PostgresFactStore, Provenance};
///
/// # async fn example() -> adk_core::Result<()> {
/// let store = PostgresFactStore::connect("postgres://localhost/app").await?;
/// store.migrate().await?;
/// let fact = MemoryFact::new(FactScope::new("app", "alice", None), FactKind::Preference,
///     "Use pnpm, not npm", FactStatus::Known, Provenance::new(FactSource::UserStated, "s1", "inv-1"));
/// let stored = store.put(fact).await?;
/// assert_eq!(store.get(&stored.id).await?.map(|fact| fact.uses), Some(stored.uses));
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "postgres")]
#[derive(Debug, Clone)]
pub struct PostgresFactStore {
    pool: sqlx::PgPool,
}

#[cfg(feature = "postgres")]
impl PostgresFactStore {
    /// A store over `pool`, such as the one the session service and the approval store use.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// Connects to `database_url` with a pool of its own.
    ///
    /// # Errors
    ///
    /// Returns `harness.store.database` when the server cannot be reached.
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = sqlx::PgPool::connect(database_url).await.map_err(crate::fsutil::db_error)?;
        Ok(Self::new(pool))
    }

    /// Creates or upgrades the store's tables.
    ///
    /// # Errors
    ///
    /// Returns the database error when a statement fails, or a version mismatch when the
    /// database was migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        crate::fsutil::migrate_postgres(
            &self.pool,
            "_adk_harness_fact_migrations",
            POSTGRES_FACT_MIGRATIONS,
        )
        .await
    }

    fn decode_fact(text: &str) -> Result<MemoryFact> {
        serde_json::from_str(text).map_err(|error| corrupt_record("fact", &error))
    }

    /// The fact `id`, its row locked until `tx` ends.
    async fn locked(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        id: &str,
    ) -> Result<Option<MemoryFact>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT record FROM adk_harness_facts WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(crate::fsutil::db_error)?;
        row.map(|(record,)| Self::decode_fact(&record)).transpose()
    }

    async fn write(
        executor: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
        fact: &MemoryFact,
    ) -> Result<()> {
        let record = serde_json::to_string(fact)
            .map_err(|error| crate::fsutil::serialize_error("fact", &error))?;
        sqlx::query(
            "INSERT INTO adk_harness_facts \
             (id, app_name, user_id, project, digest, live, valid_from, record) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (id) DO UPDATE SET live = excluded.live, record = excluded.record",
        )
        .bind(&fact.id)
        .bind(&fact.scope.app)
        .bind(&fact.scope.user)
        .bind(&fact.scope.project)
        .bind(&fact.digest)
        .bind(fact.status.is_live())
        .bind(sortable(fact.valid_from))
        .bind(record)
        .execute(executor)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }

    /// Applies `change` to the fact `id` under its row lock.
    async fn update(
        &self,
        id: &str,
        change: impl FnOnce(&mut MemoryFact) -> Result<()>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(crate::fsutil::db_error)?;
        let mut fact = Self::locked(&mut tx, id).await?.ok_or_else(|| not_found(id))?;
        change(&mut fact)?;
        Self::write(&mut *tx, &fact).await?;
        tx.commit().await.map_err(crate::fsutil::db_error)
    }
}

#[cfg(feature = "postgres")]
#[async_trait]
impl FactStore for PostgresFactStore {
    async fn put(&self, fact: MemoryFact) -> Result<MemoryFact> {
        let mut tx = self.pool.begin().await.map_err(crate::fsutil::db_error)?;
        // Two replicas storing the same fact at once keep one: the lock orders them, and the
        // second finds the first's row.
        let key = format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}",
            fact.scope.app,
            fact.scope.user,
            fact.scope.project.as_deref().unwrap_or("\u{1f}"),
            fact.digest
        );
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(key)
            .execute(&mut *tx)
            .await
            .map_err(crate::fsutil::db_error)?;
        let existing: Option<(String,)> = sqlx::query_as(
            "SELECT record FROM adk_harness_facts WHERE app_name = $1 AND user_id = $2 \
             AND project IS NOT DISTINCT FROM $3 AND digest = $4 AND live LIMIT 1 FOR UPDATE",
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
                let mut existing = Self::decode_fact(&record)?;
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
            sqlx::query_as("SELECT record FROM adk_harness_facts WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(crate::fsutil::db_error)?;
        row.map(|(record,)| Self::decode_fact(&record)).transpose()
    }

    async fn list(&self, filter: FactFilter) -> Result<Vec<MemoryFact>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT record FROM adk_harness_facts WHERE app_name = $1 AND user_id = $2 \
             AND (NOT $3 OR live)",
        )
        .bind(&filter.app)
        .bind(&filter.user)
        .bind(filter.live_only)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        let mut facts = Vec::with_capacity(rows.len());
        for (record,) in rows {
            let fact = Self::decode_fact(&record)?;
            if filter.matches(&fact) {
                facts.push(fact);
            }
        }
        sort_newest_first(&mut facts);
        Ok(facts)
    }

    async fn set_status(&self, id: &str, status: FactStatus, _by: &SubjectRef) -> Result<()> {
        self.update(id, |fact| apply_status(fact, status)).await
    }

    async fn put_embedding(&self, id: &str, embedding: FactEmbedding) -> Result<()> {
        let vector = serde_json::to_string(&embedding.vector)
            .map_err(|error| crate::fsutil::serialize_error("embedding", &error))?;
        let mut tx = self.pool.begin().await.map_err(crate::fsutil::db_error)?;
        // The fact's row lock keeps its vector from outliving a concurrent removal.
        if Self::locked(&mut tx, id).await?.is_none() {
            return Err(not_found(id));
        }
        sqlx::query(
            "INSERT INTO adk_harness_fact_embeddings (id, vector, embedder) VALUES ($1, $2, $3) \
             ON CONFLICT (id) DO UPDATE SET vector = excluded.vector, \
             embedder = excluded.embedder",
        )
        .bind(id)
        .bind(vector)
        .bind(&embedding.embedder)
        .execute(&mut *tx)
        .await
        .map_err(crate::fsutil::db_error)?;
        tx.commit().await.map_err(crate::fsutil::db_error)
    }

    async fn embeddings(&self, scope: &FactScope) -> Result<Vec<(String, FactEmbedding)>> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT e.id, e.vector, e.embedder FROM adk_harness_fact_embeddings e \
             JOIN adk_harness_facts f ON f.id = e.id \
             WHERE f.app_name = $1 AND f.user_id = $2 AND f.project IS NOT DISTINCT FROM $3 \
             AND f.live ORDER BY e.id",
        )
        .bind(&scope.app)
        .bind(&scope.user)
        .bind(&scope.project)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, vector, embedder)| {
                serde_json::from_str::<Vec<f32>>(&vector)
                    .ok()
                    .map(|vector| (id, FactEmbedding::new(embedder, vector)))
            })
            .collect())
    }

    async fn record_recall(&self, ids: &[String], at: DateTime<Utc>) -> Result<()> {
        for id in ids {
            let recalled = self
                .update(id, |fact| {
                    fact.last_recalled = Some(at);
                    fact.uses = fact.uses.saturating_add(1);
                    Ok(())
                })
                .await;
            match recalled {
                Err(error) if error.code == "harness.memory.fact_not_found" => {}
                other => other?,
            }
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
             WHERE app_name = $1 AND user_id = $2 ORDER BY session_id",
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
        sqlx::query(
            "INSERT INTO adk_harness_fact_cursors (app_name, user_id, session_id, upto) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (app_name, user_id, session_id) \
             DO UPDATE SET upto = excluded.upto WHERE excluded.upto > adk_harness_fact_cursors.upto",
        )
        .bind(app)
        .bind(user)
        .bind(session)
        .bind(sortable(upto))
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }

    async fn put_consolidation(&self, record: &ConsolidationRecord) -> Result<()> {
        let text = serde_json::to_string(record)
            .map_err(|error| crate::fsutil::serialize_error("consolidation record", &error))?;
        sqlx::query(
            "INSERT INTO adk_harness_fact_ticks (tick_id, app_name, user_id, at, record) \
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (tick_id) DO UPDATE SET record = excluded.record",
        )
        .bind(&record.tick_id)
        .bind(&record.app)
        .bind(&record.user)
        .bind(sortable(record.at))
        .bind(text)
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }

    async fn consolidations(&self, app: &str, user: &str) -> Result<Vec<ConsolidationRecord>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT record FROM adk_harness_fact_ticks \
             WHERE app_name = $1 AND user_id = $2 ORDER BY at DESC, tick_id DESC",
        )
        .bind(app)
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        rows.into_iter()
            .map(|(text,)| {
                serde_json::from_str(&text)
                    .map_err(|error| corrupt_record("consolidation record", &error))
            })
            .collect()
    }

    async fn restore_assumed(&self, id: &str, _by: &SubjectRef) -> Result<()> {
        self.update(id, apply_restore).await
    }
}
