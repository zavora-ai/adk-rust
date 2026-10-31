//! MongoDB-backed [`SubjectRegistry`] and [`ConsentStore`].
//!
//! | Collection | `_id` | Fields | Indexes |
//! |------------|-------|--------|---------|
//! | `adk_subjects` | `kind:id` | `subject`, `pseudonym`, `state`, `linked_to_key`, `linked_to`, `linked_at_us`, `created_at_us` | Unique `pseudonym`; `linked_to_key` |
//! | `adk_consent_records` | Record id | `subject_key`, `record` (JSON without `revokedAt`), `granted_at_us`, `revoked_at_us` | `(subject_key, granted_at_us, _id)` |
//!
//! | Change | Write | Why it has one winner |
//! |--------|-------|-----------------------|
//! | `ensure`, `set_state` | Upsert with `$setOnInsert` | `_id` admits one insert; a losing concurrent upsert reads the winner |
//! | `link` | `update_one` filtered on `linked_to_key: null` and `state: "active"` | The empty link is compared inside the write |
//! | `grant` | `insert_one` | `_id` admits one insert |
//! | `revoke` | `update_one` filtered on `revoked_at_us: null` | The first revocation is kept |

use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::subject::{
    ConsentRecord, ConsentStore, SubjectRecord, SubjectRegistry, SubjectState,
    consent_conflict_error, erasing_error, link_consumed_error, validate_link,
};
use adk_core::{AdkError, Clock, Result, SystemClock};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mongodb::bson::{Document, doc};
use mongodb::options::IndexOptions;
use mongodb::{Collection, Database, IndexModel};

use crate::durable_common::storage_error;
use crate::mongodb_stores::{connect, is_duplicate_key};
use crate::subject_common::{
    SUBJECT_STORAGE, StoredSubject, consent_parts, consent_record, state_name, subject_key, to_json,
};

const SUBJECTS: &str = "adk_subjects";
const CONSENT: &str = "adk_consent_records";

fn failed(error: impl std::fmt::Display) -> AdkError {
    storage_error(SUBJECT_STORAGE, format!("mongodb subject store failed: {error}"))
}

async fn open(connection_string: &str, database_name: &str) -> Result<Database> {
    connect(connection_string, database_name).await.map_err(failed)
}

async fn index(
    collection: &Collection<Document>,
    keys: Document,
    options: IndexOptions,
) -> Result<()> {
    collection
        .create_index(IndexModel::builder().keys(keys).options(options).build())
        .await
        .map(|_| ())
        .map_err(failed)
}

async fn documents(mut cursor: mongodb::Cursor<Document>) -> Result<Vec<Document>> {
    let mut documents = Vec::new();
    while cursor.advance().await.map_err(failed)? {
        documents.push(cursor.deserialize_current().map_err(failed)?);
    }
    Ok(documents)
}

fn decode_subject(document: &Document) -> Result<SubjectRecord> {
    let text = |field: &str| document.get_str(field).map(str::to_owned).map_err(failed);
    StoredSubject {
        subject: text("subject")?,
        pseudonym: text("pseudonym")?,
        state: text("state")?,
        linked_to_key: document.get_str("linked_to_key").ok().map(str::to_owned),
        linked_to: document.get_str("linked_to").ok().map(str::to_owned),
        linked_at_us: document.get_i64("linked_at_us").ok(),
        created_at_us: document.get_i64("created_at_us").map_err(failed)?,
    }
    .decode()
}

fn decode_consent(document: &Document) -> Result<ConsentRecord> {
    consent_record(
        document.get_str("record").map_err(failed)?,
        document.get_i64("revoked_at_us").ok(),
    )
}

/// MongoDB-backed [`SubjectRegistry`].
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::SubjectRegistry;
/// use adk_session::MongoSubjectRegistry;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let registry = MongoSubjectRegistry::new("mongodb://localhost:27017", "adk").await?;
/// registry.migrate().await?;
/// let record = registry.ensure(&SubjectRef::human("alice")).await?;
/// assert!(record.pseudonym.starts_with("psn:"));
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoSubjectRegistry {
    db: Database,
    clock: Arc<dyn Clock>,
}

impl MongoSubjectRegistry {
    /// Connects to `connection_string` and uses `database_name`.
    ///
    /// # Errors
    ///
    /// Returns `session.subject_store.storage` when the connection string is invalid.
    pub async fn new(connection_string: &str, database_name: &str) -> Result<Self> {
        Ok(Self::from_database(open(connection_string, database_name).await?))
    }

    /// Uses an existing database handle, such as one shared with other stores.
    pub fn from_database(db: Database) -> Self {
        Self { db, clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that stamps registration and link times.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Creates the pseudonym and link indexes.
    ///
    /// # Errors
    ///
    /// Returns `session.subject_store.storage` when an index cannot be created.
    pub async fn migrate(&self) -> Result<()> {
        let unique = IndexOptions::builder().name("pseudonym".to_owned()).unique(true).build();
        index(&self.subjects(), doc! { "pseudonym": 1 }, unique).await?;
        let linked = IndexOptions::builder().name("linked_to_key".to_owned()).build();
        index(&self.subjects(), doc! { "linked_to_key": 1 }, linked).await
    }

    fn subjects(&self) -> Collection<Document> {
        self.db.collection(SUBJECTS)
    }

    /// Registers `subject` in `state` when absent; with `overwrite`, sets the state of a
    /// registered subject too.
    async fn register(
        &self,
        subject: &SubjectRef,
        state: SubjectState,
        overwrite: bool,
    ) -> Result<()> {
        let fresh = StoredSubject::fresh(subject, state, self.clock.now())?;
        let mut on_insert = doc! {
            "subject": &fresh.subject,
            "pseudonym": &fresh.pseudonym,
            "created_at_us": fresh.created_at_us,
        };
        let update = if overwrite {
            doc! { "$set": { "state": &fresh.state }, "$setOnInsert": on_insert }
        } else {
            on_insert.insert("state", &fresh.state);
            doc! { "$setOnInsert": on_insert }
        };
        let filter = doc! { "_id": subject_key(subject) };
        match self.subjects().update_one(filter.clone(), update.clone()).upsert(true).await {
            Ok(_) => Ok(()),
            // A concurrent upsert inserted first; apply the update to its document.
            Err(error) if is_duplicate_key(&error) => {
                self.subjects().update_one(filter, update).await.map(|_| ()).map_err(failed)
            }
            Err(error) => Err(failed(error)),
        }
    }

    async fn required(&self, subject: &SubjectRef) -> Result<SubjectRecord> {
        self.get(subject)
            .await?
            .ok_or_else(|| failed(format!("a {} was written but does not read back", subject.kind)))
    }
}

#[async_trait]
impl SubjectRegistry for MongoSubjectRegistry {
    async fn get(&self, subject: &SubjectRef) -> Result<Option<SubjectRecord>> {
        self.subjects()
            .find_one(doc! { "_id": subject_key(subject) })
            .await
            .map_err(failed)?
            .as_ref()
            .map(decode_subject)
            .transpose()
    }

    async fn ensure(&self, subject: &SubjectRef) -> Result<SubjectRecord> {
        self.register(subject, SubjectState::Active, false).await?;
        self.required(subject).await
    }

    async fn by_pseudonym(&self, pseudonym: &str) -> Result<Option<SubjectRef>> {
        Ok(self
            .subjects()
            .find_one(doc! { "pseudonym": pseudonym })
            .await
            .map_err(failed)?
            .as_ref()
            .map(decode_subject)
            .transpose()?
            .map(|record| record.subject))
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
        let active = state_name(SubjectState::Active)?;
        let linked = self
            .subjects()
            .update_one(
                doc! { "_id": subject_key(visitor), "linked_to_key": null, "state": active },
                doc! { "$set": {
                    "linked_to_key": subject_key(identified),
                    "linked_to": to_json(identified)?,
                    "linked_at_us": self.clock.now().timestamp_micros(),
                } },
            )
            .await
            .map_err(failed)?;
        let record = self.required(visitor).await?;
        if linked.matched_count == 0 {
            return Err(if record.state == SubjectState::Erasing {
                erasing_error(visitor)
            } else {
                link_consumed_error()
            });
        }
        Ok(record)
    }

    async fn linked(&self, identified: &SubjectRef) -> Result<Vec<SubjectRef>> {
        let documents = documents(
            self.subjects()
                .find(doc! { "linked_to_key": subject_key(identified) })
                .await
                .map_err(failed)?,
        )
        .await?;
        let mut linked = documents
            .iter()
            .map(|document| decode_subject(document).map(|record| record.subject))
            .collect::<Result<Vec<_>>>()?;
        linked.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(linked)
    }

    async fn set_state(&self, subject: &SubjectRef, state: SubjectState) -> Result<SubjectRecord> {
        self.register(subject, state, true).await?;
        self.required(subject).await
    }

    async fn remove(&self, subject: &SubjectRef) -> Result<bool> {
        let removed = self
            .subjects()
            .delete_one(doc! { "_id": subject_key(subject) })
            .await
            .map_err(failed)?;
        Ok(removed.deleted_count > 0)
    }
}

/// MongoDB-backed [`ConsentStore`].
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::{ConsentPurpose, ConsentRecord, ConsentStore};
/// use adk_session::MongoConsentStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let consent = MongoConsentStore::new("mongodb://localhost:27017", "adk").await?;
/// consent.migrate().await?;
/// let visitor = SubjectRef::visitor("v:1");
/// consent.grant(ConsentRecord::new(visitor.clone(), ConsentPurpose::Analytics, "2026-10")).await?;
/// assert_eq!(consent.for_subject(&visitor).await?.len(), 1);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoConsentStore {
    db: Database,
}

impl MongoConsentStore {
    /// Connects to `connection_string` and uses `database_name`.
    ///
    /// # Errors
    ///
    /// Returns `session.subject_store.storage` when the connection string is invalid.
    pub async fn new(connection_string: &str, database_name: &str) -> Result<Self> {
        Ok(Self::from_database(open(connection_string, database_name).await?))
    }

    /// Uses an existing database handle, such as one shared with other stores.
    pub fn from_database(db: Database) -> Self {
        Self { db }
    }

    /// Creates the per-subject index.
    ///
    /// # Errors
    ///
    /// Returns `session.subject_store.storage` when the index cannot be created.
    pub async fn migrate(&self) -> Result<()> {
        let options = IndexOptions::builder().name("subject_granted".to_owned()).build();
        index(&self.records(), doc! { "subject_key": 1, "granted_at_us": 1, "_id": 1 }, options)
            .await
    }

    fn records(&self) -> Collection<Document> {
        self.db.collection(CONSENT)
    }
}

#[async_trait]
impl ConsentStore for MongoConsentStore {
    async fn grant(&self, record: ConsentRecord) -> Result<()> {
        let (json, granted_at_us, revoked_at_us) = consent_parts(&record)?;
        let mut document = doc! {
            "_id": &record.id,
            "subject_key": subject_key(&record.subject),
            "record": json,
            "granted_at_us": granted_at_us,
        };
        if let Some(revoked) = revoked_at_us {
            document.insert("revoked_at_us", revoked);
        }
        match self.records().insert_one(document).await {
            Ok(_) => Ok(()),
            Err(error) if is_duplicate_key(&error) => Err(consent_conflict_error(&record.id)),
            Err(error) => Err(failed(error)),
        }
    }

    async fn get(&self, id: &str) -> Result<Option<ConsentRecord>> {
        self.records()
            .find_one(doc! { "_id": id })
            .await
            .map_err(failed)?
            .as_ref()
            .map(decode_consent)
            .transpose()
    }

    async fn for_subject(&self, subject: &SubjectRef) -> Result<Vec<ConsentRecord>> {
        let documents = documents(
            self.records()
                .find(doc! { "subject_key": subject_key(subject) })
                .sort(doc! { "granted_at_us": 1, "_id": 1 })
                .await
                .map_err(failed)?,
        )
        .await?;
        documents.iter().map(decode_consent).collect()
    }

    async fn revoke(&self, id: &str, at: DateTime<Utc>) -> Result<Option<ConsentRecord>> {
        self.records()
            .update_one(
                doc! { "_id": id, "revoked_at_us": null },
                doc! { "$set": { "revoked_at_us": at.timestamp_micros() } },
            )
            .await
            .map_err(failed)?;
        self.get(id).await
    }

    async fn erase(&self, subject: &SubjectRef) -> Result<u64> {
        Ok(self
            .records()
            .delete_many(doc! { "subject_key": subject_key(subject) })
            .await
            .map_err(failed)?
            .deleted_count)
    }
}
