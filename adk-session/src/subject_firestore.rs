//! Firestore-backed [`SubjectRegistry`] and [`ConsentStore`].
//!
//! | Collection | Document ID | Fields |
//! |------------|-------------|--------|
//! | `adk_subjects` | SHA-256 of `["subject", kind:id]` | `key`, `subject`, `pseudonym`, `state`, `linked_to_key`, `linked_to`, `linked_at_us`, `created_at_us` |
//! | `adk_consent_records` | SHA-256 of `["consent", id]` | `id`, `subject_key`, `record` (JSON without `revokedAt`), `granted_at_us`, `revoked_at_us` |
//!
//! Registration creates a document only if it does not exist, so concurrent callers see one
//! pseudonym. State changes, links, and revocations run in transactions that read the
//! document they change, so a visitor id links once and a revocation keeps its first time.
//! Queries filter on one field, so no composite index is needed; consent records are
//! ordered after reading.

use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::subject::{
    ConsentRecord, ConsentStore, SubjectRecord, SubjectRegistry, SubjectState,
    consent_conflict_error, erasing_error, link_consumed_error, validate_link,
};
use adk_core::{AdkError, Clock, Result, SystemClock};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use firestore::FirestoreDb;
use firestore::errors::{BackoffError, FirestoreError};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::durable_common::storage_error;
use crate::firestore_stores::{
    TxFuture, TxResult, connect, document_id, retry_or_fail, transaction_error, transaction_options,
};
use crate::subject_common::{
    SUBJECT_STORAGE, StoredSubject, consent_parts, consent_record, sort_consent, state_name,
    subject_key, to_json,
};

const SUBJECTS: &str = "adk_subjects";
const CONSENT: &str = "adk_consent_records";

fn failed(error: impl std::fmt::Display) -> AdkError {
    storage_error(SUBJECT_STORAGE, format!("firestore subject store failed: {error}"))
}

fn tx_fail(error: FirestoreError) -> BackoffError<AdkError> {
    retry_or_fail(error, failed)
}

fn tx_error(error: FirestoreError) -> AdkError {
    transaction_error(error, failed)
}

fn permanent<T>(result: Result<T>) -> TxResult<T> {
    result.map_err(BackoffError::permanent)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SubjectDoc {
    key: String,
    subject: String,
    pseudonym: String,
    state: String,
    linked_to_key: Option<String>,
    linked_to: Option<String>,
    linked_at_us: Option<i64>,
    created_at_us: i64,
}

impl SubjectDoc {
    fn fresh(subject: &SubjectRef, state: SubjectState, at: DateTime<Utc>) -> Result<Self> {
        let stored = StoredSubject::fresh(subject, state, at)?;
        Ok(Self {
            key: subject_key(subject),
            subject: stored.subject,
            pseudonym: stored.pseudonym,
            state: stored.state,
            linked_to_key: None,
            linked_to: None,
            linked_at_us: None,
            created_at_us: stored.created_at_us,
        })
    }

    fn decode(&self) -> Result<SubjectRecord> {
        StoredSubject {
            subject: self.subject.clone(),
            pseudonym: self.pseudonym.clone(),
            state: self.state.clone(),
            linked_to_key: self.linked_to_key.clone(),
            linked_to: self.linked_to.clone(),
            linked_at_us: self.linked_at_us,
            created_at_us: self.created_at_us,
        }
        .decode()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ConsentDoc {
    id: String,
    subject_key: String,
    record: String,
    granted_at_us: i64,
    revoked_at_us: Option<i64>,
}

fn subject_id(key: &str) -> String {
    document_id(&json!(["subject", key]))
}

fn consent_id(id: &str) -> String {
    document_id(&json!(["consent", id]))
}

async fn read_subject(db: &FirestoreDb, key: &str) -> TxResult<Option<SubjectDoc>> {
    let stored: Option<SubjectDoc> = db
        .fluent()
        .select()
        .by_id_in(SUBJECTS)
        .obj::<SubjectDoc>()
        .one(subject_id(key))
        .await
        .map_err(tx_fail)?;
    Ok(stored.filter(|doc| doc.key == key))
}

async fn read_consent(db: &FirestoreDb, id: &str) -> TxResult<Option<ConsentDoc>> {
    let stored: Option<ConsentDoc> = db
        .fluent()
        .select()
        .by_id_in(CONSENT)
        .obj::<ConsentDoc>()
        .one(consent_id(id))
        .await
        .map_err(tx_fail)?;
    Ok(stored.filter(|doc| doc.id == id))
}

fn plain<T>(result: TxResult<T>) -> Result<T> {
    result.map_err(|error| match error {
        BackoffError::Permanent(error) | BackoffError::Transient { err: error, .. } => error,
    })
}

/// Firestore-backed [`SubjectRegistry`].
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::SubjectRegistry;
/// use adk_session::FirestoreSubjectRegistry;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let registry = FirestoreSubjectRegistry::new("my-project").await?;
/// let record = registry.ensure(&SubjectRef::human("alice")).await?;
/// assert!(record.pseudonym.starts_with("psn:"));
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestoreSubjectRegistry {
    db: FirestoreDb,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for FirestoreSubjectRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreSubjectRegistry")
            .field("clock", &self.clock)
            .finish_non_exhaustive()
    }
}

impl FirestoreSubjectRegistry {
    /// Connects to `project_id` with Application Default Credentials. Honors
    /// `FIRESTORE_EMULATOR_HOST`.
    ///
    /// # Errors
    ///
    /// Returns `session.subject_store.storage` when the client cannot be created.
    pub async fn new(project_id: &str) -> Result<Self> {
        Ok(Self::from_db(connect(project_id).await.map_err(failed)?))
    }

    /// Uses an existing client, such as the one a `FirestoreSessionService` holds.
    pub fn from_db(db: FirestoreDb) -> Self {
        Self { db, clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that stamps registration and link times.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Prepares the store's storage. Firestore creates collections on first write and the
    /// registry queries single fields, so there is nothing to create.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    pub async fn migrate(&self) -> Result<()> {
        Ok(())
    }

    async fn required(&self, subject: &SubjectRef) -> Result<SubjectRecord> {
        self.get(subject)
            .await?
            .ok_or_else(|| failed(format!("a {} was written but does not read back", subject.kind)))
    }

    async fn query(&self, field: &'static str, value: String) -> Result<Vec<SubjectDoc>> {
        self.db
            .fluent()
            .select()
            .from(SUBJECTS)
            .filter(|q| q.for_all([q.field(field).eq(value.as_str())]))
            .obj::<SubjectDoc>()
            .query()
            .await
            .map_err(failed)
    }
}

#[async_trait]
impl SubjectRegistry for FirestoreSubjectRegistry {
    async fn get(&self, subject: &SubjectRef) -> Result<Option<SubjectRecord>> {
        plain(read_subject(&self.db, &subject_key(subject)).await)?
            .map(|doc| doc.decode())
            .transpose()
    }

    async fn ensure(&self, subject: &SubjectRef) -> Result<SubjectRecord> {
        let doc = SubjectDoc::fresh(subject, SubjectState::Active, self.clock.now())?;
        let created = self
            .db
            .fluent()
            .insert()
            .into(SUBJECTS)
            .document_id(subject_id(&doc.key))
            .object(&doc)
            .execute::<SubjectDoc>()
            .await;
        match created {
            Ok(_) | Err(FirestoreError::DataConflictError(_)) => self.required(subject).await,
            Err(error) => Err(failed(error)),
        }
    }

    async fn by_pseudonym(&self, pseudonym: &str) -> Result<Option<SubjectRef>> {
        self.query("pseudonym", pseudonym.to_owned())
            .await?
            .into_iter()
            .find(|doc| doc.pseudonym == pseudonym)
            .map(|doc| doc.decode().map(|record| record.subject))
            .transpose()
    }

    async fn link(&self, visitor: &SubjectRef, identified: &SubjectRef) -> Result<SubjectRecord> {
        validate_link(visitor, identified)?;
        let clock = Arc::clone(&self.clock);
        let request = Arc::new((visitor.clone(), identified.clone()));
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, SubjectRecord> {
                    let (request, clock) = (Arc::clone(&request), Arc::clone(&clock));
                    Box::pin(async move {
                        let (visitor, identified) = &*request;
                        let now = clock.now();
                        let erasing = permanent(state_name(SubjectState::Erasing))?;
                        let visitor_doc = read_subject(&db, &subject_key(visitor)).await?;
                        let identified_doc = read_subject(&db, &subject_key(identified)).await?;
                        for (subject, doc) in
                            [(visitor, &visitor_doc), (identified, &identified_doc)]
                        {
                            if doc.as_ref().is_some_and(|doc| doc.state == erasing) {
                                return Err(BackoffError::permanent(erasing_error(subject)));
                            }
                        }
                        if visitor_doc.as_ref().is_some_and(|doc| doc.linked_to_key.is_some()) {
                            return Err(BackoffError::permanent(link_consumed_error()));
                        }
                        if identified_doc.is_none() {
                            let fresh = permanent(SubjectDoc::fresh(
                                identified,
                                SubjectState::Active,
                                now,
                            ))?;
                            db.fluent()
                                .update()
                                .in_col(SUBJECTS)
                                .document_id(subject_id(&fresh.key))
                                .object(&fresh)
                                .add_to_transaction(transaction)
                                .map_err(tx_fail)?;
                        }
                        let mut linked = match visitor_doc {
                            Some(doc) => doc,
                            None => {
                                permanent(SubjectDoc::fresh(visitor, SubjectState::Active, now))?
                            }
                        };
                        linked.linked_to_key = Some(subject_key(identified));
                        linked.linked_to = Some(permanent(to_json(identified))?);
                        linked.linked_at_us = Some(now.timestamp_micros());
                        db.fluent()
                            .update()
                            .in_col(SUBJECTS)
                            .document_id(subject_id(&linked.key))
                            .object(&linked)
                            .add_to_transaction(transaction)
                            .map_err(tx_fail)?;
                        permanent(linked.decode())
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(tx_error)
    }

    async fn linked(&self, identified: &SubjectRef) -> Result<Vec<SubjectRef>> {
        let key = subject_key(identified);
        let mut linked = self
            .query("linked_to_key", key.clone())
            .await?
            .iter()
            .filter(|doc| doc.linked_to_key.as_deref() == Some(key.as_str()))
            .map(|doc| doc.decode().map(|record| record.subject))
            .collect::<Result<Vec<_>>>()?;
        linked.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(linked)
    }

    async fn set_state(&self, subject: &SubjectRef, state: SubjectState) -> Result<SubjectRecord> {
        let clock = Arc::clone(&self.clock);
        let subject = Arc::new(subject.clone());
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, SubjectRecord> {
                    let (subject, clock) = (Arc::clone(&subject), Arc::clone(&clock));
                    Box::pin(async move {
                        let doc = match read_subject(&db, &subject_key(&subject)).await? {
                            Some(mut doc) => {
                                doc.state = permanent(state_name(state))?;
                                doc
                            }
                            None => permanent(SubjectDoc::fresh(&subject, state, clock.now()))?,
                        };
                        db.fluent()
                            .update()
                            .in_col(SUBJECTS)
                            .document_id(subject_id(&doc.key))
                            .object(&doc)
                            .add_to_transaction(transaction)
                            .map_err(tx_fail)?;
                        permanent(doc.decode())
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(tx_error)
    }

    async fn remove(&self, subject: &SubjectRef) -> Result<bool> {
        let key = subject_key(subject);
        if plain(read_subject(&self.db, &key).await)?.is_none() {
            return Ok(false);
        }
        self.db
            .fluent()
            .delete()
            .from(SUBJECTS)
            .document_id(subject_id(&key))
            .execute()
            .await
            .map_err(failed)?;
        Ok(true)
    }
}

/// Firestore-backed [`ConsentStore`].
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::{ConsentPurpose, ConsentRecord, ConsentStore};
/// use adk_session::FirestoreConsentStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let consent = FirestoreConsentStore::new("my-project").await?;
/// let visitor = SubjectRef::visitor("v:1");
/// consent.grant(ConsentRecord::new(visitor.clone(), ConsentPurpose::Analytics, "2026-10")).await?;
/// assert_eq!(consent.for_subject(&visitor).await?.len(), 1);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestoreConsentStore {
    db: FirestoreDb,
}

impl std::fmt::Debug for FirestoreConsentStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreConsentStore").finish_non_exhaustive()
    }
}

impl FirestoreConsentStore {
    /// Connects to `project_id` with Application Default Credentials. Honors
    /// `FIRESTORE_EMULATOR_HOST`.
    ///
    /// # Errors
    ///
    /// Returns `session.subject_store.storage` when the client cannot be created.
    pub async fn new(project_id: &str) -> Result<Self> {
        Ok(Self::from_db(connect(project_id).await.map_err(failed)?))
    }

    /// Uses an existing client, such as the one a `FirestoreSessionService` holds.
    pub fn from_db(db: FirestoreDb) -> Self {
        Self { db }
    }

    /// Prepares the store's storage. Firestore creates collections on first write and the
    /// store queries one field, so there is nothing to create.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    pub async fn migrate(&self) -> Result<()> {
        Ok(())
    }

    async fn of(&self, subject: &SubjectRef) -> Result<Vec<ConsentDoc>> {
        let key = subject_key(subject);
        let docs: Vec<ConsentDoc> = self
            .db
            .fluent()
            .select()
            .from(CONSENT)
            .filter(|q| q.for_all([q.field("subject_key").eq(key.as_str())]))
            .obj::<ConsentDoc>()
            .query()
            .await
            .map_err(failed)?;
        Ok(docs.into_iter().filter(|doc| doc.subject_key == key).collect())
    }
}

#[async_trait]
impl ConsentStore for FirestoreConsentStore {
    async fn grant(&self, record: ConsentRecord) -> Result<()> {
        let (json, granted_at_us, revoked_at_us) = consent_parts(&record)?;
        let doc = ConsentDoc {
            id: record.id.clone(),
            subject_key: subject_key(&record.subject),
            record: json,
            granted_at_us,
            revoked_at_us,
        };
        let created = self
            .db
            .fluent()
            .insert()
            .into(CONSENT)
            .document_id(consent_id(&record.id))
            .object(&doc)
            .execute::<ConsentDoc>()
            .await;
        match created {
            Ok(_) => Ok(()),
            Err(FirestoreError::DataConflictError(_)) => Err(consent_conflict_error(&record.id)),
            Err(error) => Err(failed(error)),
        }
    }

    async fn get(&self, id: &str) -> Result<Option<ConsentRecord>> {
        plain(read_consent(&self.db, id).await)?
            .map(|doc| consent_record(&doc.record, doc.revoked_at_us))
            .transpose()
    }

    async fn for_subject(&self, subject: &SubjectRef) -> Result<Vec<ConsentRecord>> {
        let mut records = self
            .of(subject)
            .await?
            .iter()
            .map(|doc| consent_record(&doc.record, doc.revoked_at_us))
            .collect::<Result<Vec<_>>>()?;
        sort_consent(&mut records);
        Ok(records)
    }

    async fn revoke(&self, id: &str, at: DateTime<Utc>) -> Result<Option<ConsentRecord>> {
        let id = Arc::new(id.to_owned());
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, Option<ConsentRecord>> {
                    let id = Arc::clone(&id);
                    Box::pin(async move {
                        let Some(mut doc) = read_consent(&db, &id).await? else {
                            return Ok(None);
                        };
                        if doc.revoked_at_us.is_none() {
                            doc.revoked_at_us = Some(at.timestamp_micros());
                            db.fluent()
                                .update()
                                .in_col(CONSENT)
                                .document_id(consent_id(&doc.id))
                                .object(&doc)
                                .add_to_transaction(transaction)
                                .map_err(tx_fail)?;
                        }
                        permanent(consent_record(&doc.record, doc.revoked_at_us)).map(Some)
                    })
                },
                transaction_options(),
            )
            .await
            .map_err(tx_error)
    }

    async fn erase(&self, subject: &SubjectRef) -> Result<u64> {
        let docs = self.of(subject).await?;
        for doc in &docs {
            self.db
                .fluent()
                .delete()
                .from(CONSENT)
                .document_id(consent_id(&doc.id))
                .execute()
                .await
                .map_err(failed)?;
        }
        Ok(docs.len() as u64)
    }
}
