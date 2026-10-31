//! The subject registry: pseudonyms, links, and erasure state.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::principal::{SubjectKind, SubjectRef};
use crate::{AdkError, Clock, ErrorCategory, ErrorComponent, Result, SystemClock};

/// The prefix of every pseudonym a registry issues.
pub const PSEUDONYM_PREFIX: &str = "psn:";

/// The error code of a write, link, or grant refused because the subject is being erased.
pub const SUBJECT_ERASING_CODE: &str = "subject.erasing";

/// The error code of a link refused because the visitor id is already linked.
pub const SUBJECT_LINK_CONSUMED_CODE: &str = "subject.link.consumed";

/// The error code of a link between subjects of the wrong kinds.
pub const SUBJECT_LINK_INVALID_CODE: &str = "subject.link.invalid";

/// Returns a new random pseudonym: [`PSEUDONYM_PREFIX`] and 32 hex characters.
///
/// # Example
///
/// ```rust
/// use adk_core::subject::new_pseudonym;
///
/// assert!(new_pseudonym().starts_with("psn:"));
/// assert_ne!(new_pseudonym(), new_pseudonym());
/// ```
pub fn new_pseudonym() -> String {
    format!("{PSEUDONYM_PREFIX}{}", uuid::Uuid::new_v4().simple())
}

/// The error returned for an action on a subject that is being erased.
pub fn erasing_error(subject: &SubjectRef) -> AdkError {
    AdkError::new(
        ErrorComponent::Auth,
        ErrorCategory::Conflict,
        SUBJECT_ERASING_CODE,
        format!(
            "{} is being erased, so nothing is written for it; finish the erasure with \
             SubjectRights::erase before acting for this subject again",
            subject.kind
        ),
    )
}

/// The error a [`SubjectRegistry`] returns when a visitor id is linked a second time.
pub fn link_consumed_error() -> AdkError {
    AdkError::new(
        ErrorComponent::Auth,
        ErrorCategory::Conflict,
        SUBJECT_LINK_CONSUMED_CODE,
        "this visitor id is already linked; a visitor id links once, so issue a new one",
    )
}

/// The error a [`SubjectRegistry`] returns when a link names subjects of the wrong kinds.
pub fn link_invalid_error(visitor: &SubjectRef, identified: &SubjectRef) -> AdkError {
    AdkError::new(
        ErrorComponent::Auth,
        ErrorCategory::InvalidInput,
        SUBJECT_LINK_INVALID_CODE,
        format!(
            "a link joins a visitor to an identified subject; got a {} and a {}",
            visitor.kind, identified.kind
        ),
    )
}

/// Where a subject stands in the registry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum SubjectState {
    /// Data may be written for the subject, subject to consent.
    #[default]
    Active,
    /// An erasure has started and not completed: nothing is written for the subject.
    Erasing,
}

/// What a registry holds about one subject. Never the subject's data.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::{SubjectRegistry, InMemorySubjectRegistry, SubjectState};
///
/// # async fn demo() -> adk_core::Result<()> {
/// let registry = InMemorySubjectRegistry::new();
/// let record = registry.ensure(&SubjectRef::human("alice")).await?;
/// assert!(record.pseudonym.starts_with("psn:"));
/// assert_eq!(record.state, SubjectState::Active);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubjectRecord {
    /// The subject.
    pub subject: SubjectRef,
    /// The random name audit records and telemetry use for the subject. Erasing the
    /// subject deletes it, so records that carry it no longer lead back to the subject.
    pub pseudonym: String,
    /// Whether the subject is being erased.
    pub state: SubjectState,
    /// The identified subject this visitor id is linked to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_to: Option<SubjectRef>,
    /// When the link was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_at: Option<DateTime<Utc>>,
    /// When the subject was registered.
    pub created_at: DateTime<Utc>,
}

impl SubjectRecord {
    /// A new active record for `subject` with a fresh pseudonym, registered at `at`.
    pub fn new(subject: SubjectRef, at: DateTime<Utc>) -> Self {
        Self {
            subject,
            pseudonym: new_pseudonym(),
            state: SubjectState::Active,
            linked_to: None,
            linked_at: None,
            created_at: at,
        }
    }
}

/// Each subject's pseudonym, links, and erasure state.
///
/// The registry holds no index of a subject's records: stores find a subject by its id.
/// Its guarantees, checked by `adk_core::subject_registry_conformance_tests!` (feature
/// `conformance`):
///
/// - [`ensure`](Self::ensure) registers a subject once; concurrent calls return one
///   pseudonym.
/// - A visitor id links to one identified subject, once ([`SUBJECT_LINK_CONSUMED_CODE`]).
/// - [`remove`](Self::remove) forgets the subject, its pseudonym, and its link.
///
/// [`InMemorySubjectRegistry`] is the reference implementation; `adk-session` ships the
/// durable ones.
#[async_trait]
pub trait SubjectRegistry: Send + Sync {
    /// Returns the record of `subject`; `None` when it was never registered or was removed.
    async fn get(&self, subject: &SubjectRef) -> Result<Option<SubjectRecord>>;

    /// Returns the record of `subject`, registering it with a fresh pseudonym when it has
    /// none.
    async fn ensure(&self, subject: &SubjectRef) -> Result<SubjectRecord>;

    /// Returns the subject `pseudonym` stands for.
    async fn by_pseudonym(&self, pseudonym: &str) -> Result<Option<SubjectRef>>;

    /// Links the visitor id `visitor` to `identified`, registering both, and returns the
    /// visitor's record.
    ///
    /// # Errors
    ///
    /// - [`SUBJECT_LINK_INVALID_CODE`] unless `visitor` is a
    ///   [`Visitor`](SubjectKind::Visitor) and `identified` is not.
    /// - [`SUBJECT_LINK_CONSUMED_CODE`] when `visitor` is already linked, to anyone.
    /// - [`SUBJECT_ERASING_CODE`] when either subject is being erased.
    async fn link(&self, visitor: &SubjectRef, identified: &SubjectRef) -> Result<SubjectRecord>;

    /// Returns the visitor ids linked to `identified`.
    async fn linked(&self, identified: &SubjectRef) -> Result<Vec<SubjectRef>>;

    /// Sets the state of `subject`, registering it when absent, and returns its record.
    async fn set_state(&self, subject: &SubjectRef, state: SubjectState) -> Result<SubjectRecord>;

    /// Forgets `subject`: its record, its pseudonym, and its link. Returns whether it was
    /// registered.
    async fn remove(&self, subject: &SubjectRef) -> Result<bool>;
}

/// Checks a link's kinds; every backend calls this before writing, so all of them refuse
/// the same links.
///
/// # Errors
///
/// [`SUBJECT_LINK_INVALID_CODE`] unless `visitor` is a visitor and `identified` is not.
pub fn validate_link(visitor: &SubjectRef, identified: &SubjectRef) -> Result<()> {
    if visitor.kind == SubjectKind::Visitor && identified.kind != SubjectKind::Visitor {
        Ok(())
    } else {
        Err(link_invalid_error(visitor, identified))
    }
}

/// In-memory [`SubjectRegistry`] for tests and single-process development.
///
/// Clones share the same records.
#[derive(Debug, Clone)]
pub struct InMemorySubjectRegistry {
    records: Arc<tokio::sync::RwLock<HashMap<SubjectRef, SubjectRecord>>>,
    clock: Arc<dyn Clock>,
}

impl Default for InMemorySubjectRegistry {
    fn default() -> Self {
        Self { records: Arc::default(), clock: Arc::new(SystemClock) }
    }
}

impl InMemorySubjectRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the clock that stamps registration and link times.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

#[async_trait]
impl SubjectRegistry for InMemorySubjectRegistry {
    async fn get(&self, subject: &SubjectRef) -> Result<Option<SubjectRecord>> {
        Ok(self.records.read().await.get(subject).cloned())
    }

    async fn ensure(&self, subject: &SubjectRef) -> Result<SubjectRecord> {
        let now = self.clock.now();
        let mut records = self.records.write().await;
        Ok(records
            .entry(subject.clone())
            .or_insert_with(|| SubjectRecord::new(subject.clone(), now))
            .clone())
    }

    async fn by_pseudonym(&self, pseudonym: &str) -> Result<Option<SubjectRef>> {
        Ok(self
            .records
            .read()
            .await
            .values()
            .find(|record| record.pseudonym == pseudonym)
            .map(|record| record.subject.clone()))
    }

    async fn link(&self, visitor: &SubjectRef, identified: &SubjectRef) -> Result<SubjectRecord> {
        validate_link(visitor, identified)?;
        let now = self.clock.now();
        let mut records = self.records.write().await;
        for subject in [visitor, identified] {
            if records.get(subject).is_some_and(|record| record.state == SubjectState::Erasing) {
                return Err(erasing_error(subject));
            }
        }
        if records.get(visitor).is_some_and(|record| record.linked_to.is_some()) {
            return Err(link_consumed_error());
        }
        records
            .entry(identified.clone())
            .or_insert_with(|| SubjectRecord::new(identified.clone(), now));
        let record = records
            .entry(visitor.clone())
            .or_insert_with(|| SubjectRecord::new(visitor.clone(), now));
        record.linked_to = Some(identified.clone());
        record.linked_at = Some(now);
        Ok(record.clone())
    }

    async fn linked(&self, identified: &SubjectRef) -> Result<Vec<SubjectRef>> {
        let mut linked: Vec<SubjectRef> = self
            .records
            .read()
            .await
            .values()
            .filter(|record| record.linked_to.as_ref() == Some(identified))
            .map(|record| record.subject.clone())
            .collect();
        linked.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(linked)
    }

    async fn set_state(&self, subject: &SubjectRef, state: SubjectState) -> Result<SubjectRecord> {
        let now = self.clock.now();
        let mut records = self.records.write().await;
        let record = records
            .entry(subject.clone())
            .or_insert_with(|| SubjectRecord::new(subject.clone(), now));
        record.state = state;
        Ok(record.clone())
    }

    async fn remove(&self, subject: &SubjectRef) -> Result<bool> {
        Ok(self.records.write().await.remove(subject).is_some())
    }
}
