//! Data subjects: visitor ids, consent, and the operations that export, erase, and link
//! one person's data across every store an agent writes.
//!
//! | Type | Role |
//! |------|------|
//! | [`VisitorId`] | A random id a deployment issues to a person it cannot otherwise identify |
//! | [`ConsentStore`] | Consent records per subject and purpose, read before a lasting write |
//! | [`SubjectRegistry`] | Each subject's pseudonym, links, and erasure state — never its data |
//! | [`SubjectDataStore`] | One store family's adapter: export and erase a subject's data |
//! | [`SubjectRights`] | Fans `export`, `erase`, and `link` out over the registry, the consent store, and every adapter |
//! | [`ConsentGate`] | Whether a subject may have something lasting written for a purpose; read by the session and memory wrappers, the tool path, and the runner |
//!
//! Stores keyed by `(app, user)` — sessions, memory, artifacts — find a subject by its
//! id. The registry holds no index of records, so no second copy of a person's data can
//! drift from the stores that hold it.
//!
//! # Erasure
//!
//! [`SubjectRights::erase`] works in this order, and can be repeated until it completes:
//!
//! 1. The subject and every visitor id linked to it move to [`SubjectState::Erasing`];
//!    consent gates refuse writes for a subject in that state.
//! 2. Every live consent record is revoked.
//! 3. Each [`SubjectDataStore`] erases the subject's data, by its
//!    [`StoreSupport`]: a store that cannot erase is reported, never skipped.
//! 4. The consent records are erased.
//! 5. An [`AuditRecordKind::Erasure`](crate::audit_log::AuditRecordKind::Erasure) record
//!    names the subject's pseudonym, never its id.
//! 6. When no store failed, the registry forgets the subject, its pseudonym, and its
//!    links. A store that failed leaves the subject `Erasing`, so a repeat finishes the job.
//!
//! The module provides mechanisms. Whether a deployment's use of them meets an obligation
//! it has is for the deployment to assess.
//!
//! # Example
//!
//! ```rust
//! use std::sync::Arc;
//! use adk_core::subject::{
//!     ConsentPurpose, ConsentRecord, ConsentStore, ErasureStatus, InMemoryConsentStore,
//!     InMemorySubjectRegistry, SubjectRights, VisitorId,
//! };
//!
//! # async fn demo() -> adk_core::Result<()> {
//! let rights = SubjectRights::new(
//!     Arc::new(InMemorySubjectRegistry::new()),
//!     Arc::new(InMemoryConsentStore::new()),
//! )
//! .with_app("shop");
//!
//! let visitor = VisitorId::generate().subject();
//! rights
//!     .grant(ConsentRecord::new(visitor.clone(), ConsentPurpose::PersistentProfile, "2026-10"))
//!     .await?;
//! assert!(rights.consent().live(&visitor, &ConsentPurpose::PersistentProfile, chrono::Utc::now()).await?.is_some());
//!
//! let report = rights.erase(&visitor).await?;
//! assert_eq!(report.status, ErasureStatus::Completed);
//! assert!(rights.consent().for_subject(&visitor).await?.is_empty());
//! # Ok(())
//! # }
//! ```

mod consent;
mod gate;
mod registry;
mod rights;

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::principal::SubjectRef;

pub use consent::{
    CONSENT_CONFLICT_CODE, ConsentEvidence, ConsentPurpose, ConsentRecord, ConsentStore,
    InMemoryConsentStore, consent_conflict_error,
};
pub use gate::{CONSENT_REQUIRED_CODE, ConsentGate, consent_required_error};
pub use registry::{
    InMemorySubjectRegistry, PSEUDONYM_PREFIX, SUBJECT_ERASING_CODE, SUBJECT_LINK_CONSUMED_CODE,
    SUBJECT_LINK_INVALID_CODE, SubjectRecord, SubjectRegistry, SubjectState, erasing_error,
    link_consumed_error, link_invalid_error, new_pseudonym, validate_link,
};
pub use rights::{
    ErasureReport, ErasureStatus, ExportOutcome, Revocation, SUBJECT_UNSUPPORTED_CODE,
    StoreErasure, StoreExport, StoreOutcome, StoreReport, StoreSupport, SubjectDataStore,
    SubjectExport, SubjectRights, unsupported_error,
};

/// The prefix of every [`VisitorId`].
pub const VISITOR_ID_PREFIX: &str = "v:";

/// Error returned when a string is not a [`VisitorId`].
///
/// # Example
///
/// ```rust
/// use adk_core::subject::VisitorId;
///
/// assert!("v:not-random".parse::<VisitorId>().is_err());
/// ```
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error(
    "'{0}' is not a visitor id; visitor ids are `v:` followed by 64 lowercase hex characters \
     and come from VisitorId::generate"
)]
pub struct InvalidVisitorId(pub String);

/// A random identifier a deployment issues to a person it knows nothing else about, such
/// as a website visitor who has not signed in.
///
/// `v:` followed by 64 lowercase hex characters drawn from two random UUIDs (244 random
/// bits). An id is never derived from an address, a user agent, or a device fingerprint:
/// [`VisitorId::generate`] is the only way to make one, and parsing accepts only that form.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectKind;
/// use adk_core::subject::VisitorId;
///
/// let id = VisitorId::generate();
/// assert_eq!(id.as_str().parse::<VisitorId>(), Ok(id.clone()));
/// assert_eq!(id.subject().kind, SubjectKind::Visitor);
/// assert_ne!(VisitorId::generate(), id);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct VisitorId(String);

impl VisitorId {
    /// Generates a new random visitor id.
    pub fn generate() -> Self {
        Self(format!(
            "{VISITOR_ID_PREFIX}{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    /// Returns the id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The subject this id names, of kind [`SubjectKind::Visitor`].
    pub fn subject(&self) -> SubjectRef {
        SubjectRef::visitor(self.0.clone())
    }
}

impl fmt::Display for VisitorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for VisitorId {
    type Err = InvalidVisitorId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let valid = s.strip_prefix(VISITOR_ID_PREFIX).is_some_and(|hex| {
            hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
        if valid { Ok(Self(s.to_owned())) } else { Err(InvalidVisitorId(s.to_owned())) }
    }
}

impl TryFrom<String> for VisitorId {
    type Error = InvalidVisitorId;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<VisitorId> for String {
    fn from(id: VisitorId) -> Self {
        id.0
    }
}

/// Whose data an export or an erasure covers: a subject, the visitor ids linked to it,
/// and the applications whose `(app, user)`-keyed stores hold it.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::SubjectScope;
///
/// let scope = SubjectScope::new(["shop"], SubjectRef::human("alice"))
///     .with_linked(vec![SubjectRef::visitor("v:1"), SubjectRef::human("alice")]);
/// assert_eq!(scope.user_ids(), ["alice", "v:1"]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubjectScope {
    /// The applications whose stores are searched.
    pub app_names: Vec<String>,
    /// The subject.
    pub subject: SubjectRef,
    /// The visitor ids linked to the subject; data held under them is the subject's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked: Vec<SubjectRef>,
}

impl SubjectScope {
    /// The scope of `subject` alone in `app_names`.
    pub fn new<I, S>(app_names: I, subject: SubjectRef) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            app_names: app_names.into_iter().map(Into::into).collect(),
            subject,
            linked: Vec::new(),
        }
    }

    /// Sets the linked visitor ids.
    #[must_use]
    pub fn with_linked(mut self, linked: Vec<SubjectRef>) -> Self {
        self.linked = linked;
        self
    }

    /// The subject followed by its linked ids.
    pub fn subjects(&self) -> impl Iterator<Item = &SubjectRef> {
        std::iter::once(&self.subject).chain(self.linked.iter())
    }

    /// The user ids that `(app, user)`-keyed stores hold the subject's data under: the
    /// subject's id, then each linked id, without repeats.
    pub fn user_ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = Vec::with_capacity(1 + self.linked.len());
        for subject in self.subjects() {
            if !ids.contains(&subject.id.as_str()) {
                ids.push(&subject.id);
            }
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visitor_ids_are_random_and_parse_only_their_own_form() {
        let id = VisitorId::generate();
        assert_eq!(id.as_str().len(), 66);
        assert!(id.as_str().starts_with("v:"));
        assert_eq!(VisitorId::from_str(id.as_str()), Ok(id.clone()));
        assert_ne!(VisitorId::generate(), id);
        for bad in
            ["", "v:", "v:abc", &format!("x:{}", &id.as_str()[2..]), &id.as_str().to_uppercase()]
        {
            assert!(VisitorId::from_str(bad).is_err(), "{bad} is not a visitor id");
        }
        let json = serde_json::to_value(&id).unwrap();
        assert_eq!(serde_json::from_value::<VisitorId>(json).unwrap(), id);
        assert!(serde_json::from_value::<VisitorId>(serde_json::json!("v:1")).is_err());
    }

    #[test]
    fn scope_lists_each_user_id_once() {
        let scope = SubjectScope::new(["a", "b"], SubjectRef::human("alice")).with_linked(vec![
            SubjectRef::visitor("v:1"),
            SubjectRef::visitor("v:2"),
            SubjectRef::visitor("v:1"),
        ]);
        assert_eq!(scope.user_ids(), ["alice", "v:1", "v:2"]);
        assert_eq!(scope.subjects().count(), 4);
    }
}
