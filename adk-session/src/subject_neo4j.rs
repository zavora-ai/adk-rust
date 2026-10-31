//! Neo4j-backed [`SubjectRegistry`] and [`ConsentStore`].
//!
//! | Label | Properties | Constraints |
//! |-------|------------|-------------|
//! | `AdkSubject` | `key` (`kind:id`), `subject`, `pseudonym`, `state`, `linked_to_key`, `linked_to`, `linked_at_us`, `created_at_us` | Unique `key`; unique `pseudonym`; index on `linked_to_key` |
//! | `AdkConsent` | `id`, `subject_key`, `record` (JSON without `revokedAt`), `granted_at_us`, `revoked_at_us` | Unique `id`; index on `subject_key` |
//!
//! Registration is a `MERGE` on the unique key; a concurrent `MERGE` that loses the
//! constraint race retries and matches the winner. A link and a revocation first take the
//! node's write lock (`SET n.lock_seq = ...`) inside a transaction and then read it, so a
//! concurrent link or revocation waits and sees the first one's result.
//!
//! Every call runs to completion on its own task, as the other Neo4j stores do, because
//! neo4rs connections are not cancellation-safe.

use adk_core::principal::SubjectRef;
use adk_core::subject::{
    ConsentRecord, ConsentStore, SubjectRecord, SubjectRegistry, SubjectState,
    consent_conflict_error, erasing_error, link_consumed_error, validate_link,
};
use adk_core::{AdkError, Clock, Result, SystemClock};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use neo4rs::{Graph, Query, Row, query};
use std::sync::Arc;

use crate::durable_common::storage_error;
use crate::neo4j_stores::{
    CONSTRAINT_VIOLATION, Failure, Inline, column, connect, detached, graph_rows, neo4j_code, rows,
    run_schema,
};
use crate::subject_common::{
    SUBJECT_STORAGE, StoredSubject, consent_parts, consent_record, sort_consent, state_name,
    subject_key, to_json,
};

/// Attempts at a registration that loses a concurrent constraint race.
const REGISTER_ATTEMPTS: usize = 5;

fn failed(message: String) -> AdkError {
    storage_error(SUBJECT_STORAGE, format!("neo4j subject store failed: {message}"))
}

fn database(error: neo4rs::Error) -> AdkError {
    failed(error.to_string())
}

const SUBJECT_COLUMNS: &str = "s.subject AS subject, s.pseudonym AS pseudonym, \
    s.state AS state, s.linked_to_key AS linked_to_key, s.linked_to AS linked_to, \
    s.linked_at_us AS linked_at_us, s.created_at_us AS created_at_us";

fn decode_subject(row: &Row) -> Result<SubjectRecord> {
    StoredSubject {
        subject: column(row, "subject").map_err(failed)?,
        pseudonym: column(row, "pseudonym").map_err(failed)?,
        state: column(row, "state").map_err(failed)?,
        linked_to_key: column(row, "linked_to_key").map_err(failed)?,
        linked_to: column(row, "linked_to").map_err(failed)?,
        linked_at_us: column(row, "linked_at_us").map_err(failed)?,
        created_at_us: column(row, "created_at_us").map_err(failed)?,
    }
    .decode()
}

fn decode_consent(row: &Row) -> Result<ConsentRecord> {
    consent_record(
        &column::<String>(row, "record").map_err(failed)?,
        column(row, "revoked_at_us").map_err(failed)?,
    )
}

/// Neo4j-backed [`SubjectRegistry`].
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::SubjectRegistry;
/// use adk_session::Neo4jSubjectRegistry;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let registry = Neo4jSubjectRegistry::new("bolt://localhost:7687", "neo4j", "password").await?;
/// registry.migrate().await?;
/// let record = registry.ensure(&SubjectRef::human("alice")).await?;
/// assert!(record.pseudonym.starts_with("psn:"));
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Neo4jSubjectRegistry {
    graph: Graph,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Neo4jSubjectRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jSubjectRegistry").field("clock", &self.clock).finish_non_exhaustive()
    }
}

impl Neo4jSubjectRegistry {
    /// Connects to `uri` as `user`.
    ///
    /// # Errors
    ///
    /// Returns `session.subject_store.storage` when the server cannot be reached.
    pub async fn new(uri: &str, user: &str, password: &str) -> Result<Self> {
        Ok(Self::from_graph(connect(uri, user, password, failed).await?))
    }

    /// Uses an existing graph handle, such as one shared with other stores.
    pub fn from_graph(graph: Graph) -> Self {
        Self { graph, clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that stamps registration and link times.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Creates the key and pseudonym constraints and the link index.
    ///
    /// # Errors
    ///
    /// Returns `session.subject_store.storage` when a schema statement fails.
    pub async fn migrate(&self) -> Result<()> {
        run_schema(
            &self.graph,
            &[
                "CREATE CONSTRAINT adk_subject_key_unique IF NOT EXISTS \
                 FOR (s:AdkSubject) REQUIRE s.key IS UNIQUE",
                "CREATE CONSTRAINT adk_subject_pseudonym_unique IF NOT EXISTS \
                 FOR (s:AdkSubject) REQUIRE s.pseudonym IS UNIQUE",
                "CREATE INDEX adk_subject_linked_to IF NOT EXISTS \
                 FOR (s:AdkSubject) ON (s.linked_to_key)",
            ],
            failed,
        )
        .await
    }
}

impl Inline<Neo4jSubjectRegistry> {
    async fn one(&self, statement: Query) -> Result<Option<SubjectRecord>> {
        let found = graph_rows(&self.graph, statement).await.map_err(database)?;
        found.first().map(decode_subject).transpose()
    }

    async fn subjects(&self, statement: Query) -> Result<Vec<SubjectRef>> {
        let found = graph_rows(&self.graph, statement).await.map_err(database)?;
        found
            .iter()
            .map(|row| {
                let text: String = column(row, "subject").map_err(failed)?;
                serde_json::from_str(&text)
                    .map_err(|e| failed(format!("stored subject is corrupt: {e}")))
            })
            .collect()
    }

    /// Registers `subject` in `state` when absent; with `overwrite`, sets the state of a
    /// registered subject too. Returns its record.
    async fn register(
        &self,
        subject: &SubjectRef,
        state: SubjectState,
        overwrite: bool,
    ) -> Result<SubjectRecord> {
        let on_match = if overwrite { "ON MATCH SET s.state = $state " } else { "" };
        let mut tries = 1;
        loop {
            let fresh = StoredSubject::fresh(subject, state, self.clock.now())?;
            let statement = query(&format!(
                "MERGE (s:AdkSubject {{key: $key}}) \
                 ON CREATE SET s.subject = $subject, s.pseudonym = $pseudonym, \
                     s.state = $state, s.created_at_us = $created_at_us \
                 {on_match}RETURN {SUBJECT_COLUMNS}"
            ))
            .param("key", subject_key(subject))
            .param("subject", fresh.subject)
            .param("pseudonym", fresh.pseudonym)
            .param("state", fresh.state)
            .param("created_at_us", fresh.created_at_us);
            match graph_rows(&self.graph, statement).await {
                Ok(found) => {
                    return found
                        .first()
                        .map(decode_subject)
                        .transpose()?
                        .ok_or_else(|| failed("a registered subject did not read back".into()));
                }
                // A concurrent MERGE created the node first; the next attempt matches it.
                Err(error)
                    if neo4j_code(&error) == Some(CONSTRAINT_VIOLATION)
                        && tries < REGISTER_ATTEMPTS =>
                {
                    tries += 1;
                }
                Err(error) => return Err(database(error)),
            }
        }
    }

    async fn try_link(
        &self,
        visitor: &SubjectRef,
        identified: &SubjectRef,
    ) -> std::result::Result<(), Failure> {
        let (visitor_key, identified_key) = (subject_key(visitor), subject_key(identified));
        let erasing = state_name(SubjectState::Erasing)?;
        let now_us = self.clock.now().timestamp_micros();
        let mut txn = self.graph.start_txn().await?;
        let result: std::result::Result<(), Failure> = async {
            txn.run(
                query(
                    "MATCH (v:AdkSubject {key: $key}) SET v.lock_seq = coalesce(v.lock_seq, 0) + 1",
                )
                .param("key", visitor_key.as_str()),
            )
            .await?;
            let found = rows(
                &mut txn,
                query(
                    "MATCH (v:AdkSubject {key: $visitor}), (i:AdkSubject {key: $identified}) \
                     RETURN v.state AS visitor_state, v.linked_to_key AS linked_to_key, \
                         i.state AS identified_state",
                )
                .param("visitor", visitor_key.as_str())
                .param("identified", identified_key.as_str()),
            )
            .await?;
            let row =
                found.first().ok_or_else(|| failed("linked subjects did not read back".into()))?;
            let visitor_state: String = column(row, "visitor_state").map_err(failed)?;
            let identified_state: String = column(row, "identified_state").map_err(failed)?;
            let linked_to: Option<String> = column(row, "linked_to_key").map_err(failed)?;
            if visitor_state == erasing {
                return Err(erasing_error(visitor).into());
            }
            if identified_state == erasing {
                return Err(erasing_error(identified).into());
            }
            if linked_to.is_some() {
                return Err(link_consumed_error().into());
            }
            txn.run(
                query(
                    "MATCH (v:AdkSubject {key: $key}) \
                     SET v.linked_to_key = $linked_to_key, v.linked_to = $linked_to, \
                         v.linked_at_us = $linked_at_us",
                )
                .param("key", visitor_key.as_str())
                .param("linked_to_key", identified_key.as_str())
                .param("linked_to", to_json(identified)?)
                .param("linked_at_us", now_us),
            )
            .await?;
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                txn.commit().await?;
                Ok(())
            }
            Err(failure) => {
                let _ = txn.rollback().await;
                Err(failure)
            }
        }
    }
}

#[async_trait]
impl SubjectRegistry for Inline<Neo4jSubjectRegistry> {
    async fn get(&self, subject: &SubjectRef) -> Result<Option<SubjectRecord>> {
        self.one(
            query(&format!("MATCH (s:AdkSubject {{key: $key}}) RETURN {SUBJECT_COLUMNS}"))
                .param("key", subject_key(subject)),
        )
        .await
    }

    async fn ensure(&self, subject: &SubjectRef) -> Result<SubjectRecord> {
        self.register(subject, SubjectState::Active, false).await
    }

    async fn by_pseudonym(&self, pseudonym: &str) -> Result<Option<SubjectRef>> {
        Ok(self
            .subjects(
                query("MATCH (s:AdkSubject {pseudonym: $pseudonym}) RETURN s.subject AS subject")
                    .param("pseudonym", pseudonym),
            )
            .await?
            .into_iter()
            .next())
    }

    async fn link(&self, visitor: &SubjectRef, identified: &SubjectRef) -> Result<SubjectRecord> {
        validate_link(visitor, identified)?;
        for subject in [visitor, identified] {
            if self.get(subject).await?.is_some_and(|record| record.state == SubjectState::Erasing)
            {
                return Err(erasing_error(subject));
            }
        }
        if self.get(visitor).await?.is_some_and(|record| record.linked_to.is_some()) {
            return Err(link_consumed_error());
        }
        self.register(identified, SubjectState::Active, false).await?;
        self.register(visitor, SubjectState::Active, false).await?;
        self.try_link(visitor, identified).await.map_err(|failure| failure.into_adk(failed))?;
        self.get(visitor).await?.ok_or_else(|| failed("a linked visitor did not read back".into()))
    }

    async fn linked(&self, identified: &SubjectRef) -> Result<Vec<SubjectRef>> {
        let mut linked = self
            .subjects(
                query("MATCH (s:AdkSubject {linked_to_key: $key}) RETURN s.subject AS subject")
                    .param("key", subject_key(identified)),
            )
            .await?;
        linked.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(linked)
    }

    async fn set_state(&self, subject: &SubjectRef, state: SubjectState) -> Result<SubjectRecord> {
        self.register(subject, state, true).await
    }

    async fn remove(&self, subject: &SubjectRef) -> Result<bool> {
        let found = graph_rows(
            &self.graph,
            query(
                "MATCH (s:AdkSubject {key: $key}) WITH s, s.key AS key DETACH DELETE s \
                 RETURN count(key) AS removed",
            )
            .param("key", subject_key(subject)),
        )
        .await
        .map_err(database)?;
        let removed: i64 =
            found.first().map_or(Ok(0), |row| column(row, "removed")).map_err(failed)?;
        Ok(removed > 0)
    }
}

#[async_trait]
impl SubjectRegistry for Neo4jSubjectRegistry {
    async fn get(&self, subject: &SubjectRef) -> Result<Option<SubjectRecord>> {
        let (inline, subject) = (Inline(self.clone()), subject.clone());
        detached(async move { inline.get(&subject).await }).await
    }

    async fn ensure(&self, subject: &SubjectRef) -> Result<SubjectRecord> {
        let (inline, subject) = (Inline(self.clone()), subject.clone());
        detached(async move { inline.ensure(&subject).await }).await
    }

    async fn by_pseudonym(&self, pseudonym: &str) -> Result<Option<SubjectRef>> {
        let (inline, pseudonym) = (Inline(self.clone()), pseudonym.to_owned());
        detached(async move { inline.by_pseudonym(&pseudonym).await }).await
    }

    async fn link(&self, visitor: &SubjectRef, identified: &SubjectRef) -> Result<SubjectRecord> {
        let (inline, visitor, identified) =
            (Inline(self.clone()), visitor.clone(), identified.clone());
        detached(async move { inline.link(&visitor, &identified).await }).await
    }

    async fn linked(&self, identified: &SubjectRef) -> Result<Vec<SubjectRef>> {
        let (inline, identified) = (Inline(self.clone()), identified.clone());
        detached(async move { inline.linked(&identified).await }).await
    }

    async fn set_state(&self, subject: &SubjectRef, state: SubjectState) -> Result<SubjectRecord> {
        let (inline, subject) = (Inline(self.clone()), subject.clone());
        detached(async move { inline.set_state(&subject, state).await }).await
    }

    async fn remove(&self, subject: &SubjectRef) -> Result<bool> {
        let (inline, subject) = (Inline(self.clone()), subject.clone());
        detached(async move { inline.remove(&subject).await }).await
    }
}

/// Neo4j-backed [`ConsentStore`].
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::{ConsentPurpose, ConsentRecord, ConsentStore};
/// use adk_session::Neo4jConsentStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let consent = Neo4jConsentStore::new("bolt://localhost:7687", "neo4j", "password").await?;
/// consent.migrate().await?;
/// let visitor = SubjectRef::visitor("v:1");
/// consent.grant(ConsentRecord::new(visitor.clone(), ConsentPurpose::Analytics, "2026-10")).await?;
/// assert_eq!(consent.for_subject(&visitor).await?.len(), 1);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Neo4jConsentStore {
    graph: Graph,
}

impl std::fmt::Debug for Neo4jConsentStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jConsentStore").finish_non_exhaustive()
    }
}

impl Neo4jConsentStore {
    /// Connects to `uri` as `user`.
    ///
    /// # Errors
    ///
    /// Returns `session.subject_store.storage` when the server cannot be reached.
    pub async fn new(uri: &str, user: &str, password: &str) -> Result<Self> {
        Ok(Self::from_graph(connect(uri, user, password, failed).await?))
    }

    /// Uses an existing graph handle, such as one shared with other stores.
    pub fn from_graph(graph: Graph) -> Self {
        Self { graph }
    }

    /// Creates the id constraint and the per-subject index.
    ///
    /// # Errors
    ///
    /// Returns `session.subject_store.storage` when a schema statement fails.
    pub async fn migrate(&self) -> Result<()> {
        run_schema(
            &self.graph,
            &[
                "CREATE CONSTRAINT adk_consent_id_unique IF NOT EXISTS \
                 FOR (c:AdkConsent) REQUIRE c.id IS UNIQUE",
                "CREATE INDEX adk_consent_subject IF NOT EXISTS \
                 FOR (c:AdkConsent) ON (c.subject_key)",
            ],
            failed,
        )
        .await
    }
}

const CONSENT_COLUMNS: &str = "c.record AS record, c.revoked_at_us AS revoked_at_us";

impl Inline<Neo4jConsentStore> {
    async fn records(&self, statement: Query) -> Result<Vec<ConsentRecord>> {
        let found = graph_rows(&self.graph, statement).await.map_err(database)?;
        found.iter().map(decode_consent).collect()
    }

    async fn try_revoke(
        &self,
        id: &str,
        at: DateTime<Utc>,
    ) -> std::result::Result<Option<ConsentRecord>, Failure> {
        let mut txn = self.graph.start_txn().await?;
        let result: std::result::Result<Option<ConsentRecord>, Failure> =
            async {
                txn.run(
                query("MATCH (c:AdkConsent {id: $id}) SET c.lock_seq = coalesce(c.lock_seq, 0) + 1")
                    .param("id", id),
            )
            .await?;
                let found = rows(
                    &mut txn,
                    query(&format!(
                        "MATCH (c:AdkConsent {{id: $id}}) \
                     SET c.revoked_at_us = coalesce(c.revoked_at_us, $at) RETURN {CONSENT_COLUMNS}"
                    ))
                    .param("id", id)
                    .param("at", at.timestamp_micros()),
                )
                .await?;
                Ok(found.first().map(decode_consent).transpose()?)
            }
            .await;
        match result {
            Ok(record) => {
                txn.commit().await?;
                Ok(record)
            }
            Err(failure) => {
                let _ = txn.rollback().await;
                Err(failure)
            }
        }
    }
}

#[async_trait]
impl ConsentStore for Inline<Neo4jConsentStore> {
    async fn grant(&self, record: ConsentRecord) -> Result<()> {
        let (json, granted_at_us, revoked_at_us) = consent_parts(&record)?;
        let revoked = if revoked_at_us.is_some() { ", revoked_at_us: $revoked_at_us" } else { "" };
        let mut statement = query(&format!(
            "CREATE (:AdkConsent {{id: $id, subject_key: $subject_key, record: $record, \
             granted_at_us: $granted_at_us{revoked}}})"
        ))
        .param("id", record.id.as_str())
        .param("subject_key", subject_key(&record.subject))
        .param("record", json)
        .param("granted_at_us", granted_at_us);
        if let Some(revoked_at_us) = revoked_at_us {
            statement = statement.param("revoked_at_us", revoked_at_us);
        }
        match self.graph.run(statement).await {
            Ok(()) => Ok(()),
            Err(error) if neo4j_code(&error) == Some(CONSTRAINT_VIOLATION) => {
                Err(consent_conflict_error(&record.id))
            }
            Err(error) => Err(database(error)),
        }
    }

    async fn get(&self, id: &str) -> Result<Option<ConsentRecord>> {
        Ok(self
            .records(
                query(&format!("MATCH (c:AdkConsent {{id: $id}}) RETURN {CONSENT_COLUMNS}"))
                    .param("id", id),
            )
            .await?
            .into_iter()
            .next())
    }

    async fn for_subject(&self, subject: &SubjectRef) -> Result<Vec<ConsentRecord>> {
        let mut records = self
            .records(
                query(&format!(
                    "MATCH (c:AdkConsent {{subject_key: $key}}) RETURN {CONSENT_COLUMNS}"
                ))
                .param("key", subject_key(subject)),
            )
            .await?;
        sort_consent(&mut records);
        Ok(records)
    }

    async fn revoke(&self, id: &str, at: DateTime<Utc>) -> Result<Option<ConsentRecord>> {
        self.try_revoke(id, at).await.map_err(|failure| failure.into_adk(failed))
    }

    async fn erase(&self, subject: &SubjectRef) -> Result<u64> {
        let found = graph_rows(
            &self.graph,
            query(
                "MATCH (c:AdkConsent {subject_key: $key}) WITH c, c.id AS id DELETE c \
                 RETURN count(id) AS erased",
            )
            .param("key", subject_key(subject)),
        )
        .await
        .map_err(database)?;
        let erased: i64 =
            found.first().map_or(Ok(0), |row| column(row, "erased")).map_err(failed)?;
        u64::try_from(erased).map_err(|_| failed(format!("erase counted {erased}")))
    }
}

#[async_trait]
impl ConsentStore for Neo4jConsentStore {
    async fn grant(&self, record: ConsentRecord) -> Result<()> {
        let inline = Inline(self.clone());
        detached(async move { inline.grant(record).await }).await
    }

    async fn get(&self, id: &str) -> Result<Option<ConsentRecord>> {
        let (inline, id) = (Inline(self.clone()), id.to_owned());
        detached(async move { inline.get(&id).await }).await
    }

    async fn for_subject(&self, subject: &SubjectRef) -> Result<Vec<ConsentRecord>> {
        let (inline, subject) = (Inline(self.clone()), subject.clone());
        detached(async move { inline.for_subject(&subject).await }).await
    }

    async fn revoke(&self, id: &str, at: DateTime<Utc>) -> Result<Option<ConsentRecord>> {
        let (inline, id) = (Inline(self.clone()), id.to_owned());
        detached(async move { inline.revoke(&id, at).await }).await
    }

    async fn erase(&self, subject: &SubjectRef) -> Result<u64> {
        let (inline, subject) = (Inline(self.clone()), subject.clone());
        detached(async move { inline.erase(&subject).await }).await
    }
}
