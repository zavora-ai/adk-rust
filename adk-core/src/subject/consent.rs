//! Consent records and the store that keeps them.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::principal::SubjectRef;
use crate::{AdkError, ErrorCategory, ErrorComponent, Result};

/// The error code of a grant whose id another consent record already has.
pub const CONSENT_CONFLICT_CODE: &str = "consent.conflict";

/// What a subject consents to: the purpose a lasting write serves.
///
/// The names match AWP's consent scopes one to one, so an AWP receipt records consent
/// for the purpose of the same name. Any other name is [`Custom`](Self::Custom).
///
/// | Purpose | Lasting writes it allows |
/// |---------|--------------------------|
/// | [`SessionContinuity`](Self::SessionContinuity) | A conversation kept beyond the visit |
/// | [`PersistentProfile`](Self::PersistentProfile) | The `user:` state tier and long-term memory |
/// | [`Analytics`](Self::Analytics) | User attributes on telemetry; visit counting |
/// | [`ProactiveOutreach`](Self::ProactiveOutreach) | Messages the site starts |
/// | [`PaymentData`](Self::PaymentData) | Stored payment preferences |
/// | [`Newsletter`](Self::Newsletter) | The site's newsletter |
///
/// # Example
///
/// ```rust
/// use adk_core::subject::ConsentPurpose;
///
/// assert_eq!(ConsentPurpose::PersistentProfile.as_str(), "persistent_profile");
/// assert_eq!(ConsentPurpose::from("analytics"), ConsentPurpose::Analytics);
/// assert_eq!(ConsentPurpose::from("contact"), ConsentPurpose::Custom("contact".into()));
/// assert_eq!(serde_json::to_value(ConsentPurpose::Newsletter).unwrap(), "newsletter");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
#[non_exhaustive]
pub enum ConsentPurpose {
    /// Keep a conversation beyond the visit and link it across channels.
    SessionContinuity,
    /// Keep data about the subject beyond the session: the `user:` tier and memory.
    PersistentProfile,
    /// Measure use: user attributes on telemetry and visit counts.
    Analytics,
    /// Send messages the subject did not ask for in the moment.
    ProactiveOutreach,
    /// Store payment preferences.
    PaymentData,
    /// Send the site's newsletter.
    Newsletter,
    /// A purpose the deployment defines.
    Custom(String),
}

impl ConsentPurpose {
    /// Returns the wire name.
    pub fn as_str(&self) -> &str {
        match self {
            Self::SessionContinuity => "session_continuity",
            Self::PersistentProfile => "persistent_profile",
            Self::Analytics => "analytics",
            Self::ProactiveOutreach => "proactive_outreach",
            Self::PaymentData => "payment_data",
            Self::Newsletter => "newsletter",
            Self::Custom(name) => name,
        }
    }
}

impl fmt::Display for ConsentPurpose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for ConsentPurpose {
    fn from(value: &str) -> Self {
        match value {
            "session_continuity" => Self::SessionContinuity,
            "persistent_profile" => Self::PersistentProfile,
            "analytics" => Self::Analytics,
            "proactive_outreach" => Self::ProactiveOutreach,
            "payment_data" => Self::PaymentData,
            "newsletter" => Self::Newsletter,
            other => Self::Custom(other.to_owned()),
        }
    }
}

impl From<String> for ConsentPurpose {
    fn from(value: String) -> Self {
        match Self::from(value.as_str()) {
            Self::Custom(_) => Self::Custom(value),
            known => known,
        }
    }
}

impl From<ConsentPurpose> for String {
    fn from(value: ConsentPurpose) -> Self {
        match value {
            ConsentPurpose::Custom(name) => name,
            known => known.as_str().to_owned(),
        }
    }
}

/// What shows that consent was given, held in the system that issued it.
///
/// # Example
///
/// ```rust
/// use adk_core::subject::ConsentEvidence;
///
/// let evidence = ConsentEvidence::new("awp.receipt", "0b6f…");
/// assert!(evidence.document.is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsentEvidence {
    /// What the evidence is, such as `awp.receipt` for a signed AWP consent receipt.
    pub kind: String,
    /// Its identifier where it is held, such as the receipt id.
    pub reference: String,
    /// A copy of the evidence, such as the signed receipt, when the record carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<Value>,
}

impl ConsentEvidence {
    /// Evidence of `kind` held under `reference`, with no copy.
    pub fn new(kind: impl Into<String>, reference: impl Into<String>) -> Self {
        Self { kind: kind.into(), reference: reference.into(), document: None }
    }

    /// Attaches a copy of the evidence.
    #[must_use]
    pub fn with_document(mut self, document: Value) -> Self {
        self.document = Some(document);
        self
    }
}

/// One grant of consent by a subject for a purpose, under a policy version.
///
/// A record is live from `granted_at` until it expires or is revoked. Revocation is kept
/// on the record, so the history of a subject's consent reads back until the subject is
/// erased.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::{ConsentPurpose, ConsentRecord};
/// use chrono::{Duration, Utc};
///
/// let now = Utc::now();
/// let record = ConsentRecord::new(SubjectRef::visitor("v:1"), ConsentPurpose::Analytics, "2026-10")
///     .granted_at(now)
///     .with_expires_at(now + Duration::days(30));
/// assert!(record.is_live(now));
/// assert!(!record.is_live(now + Duration::days(31)));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsentRecord {
    /// The record's id.
    pub id: String,
    /// Who consented.
    pub subject: SubjectRef,
    /// What for.
    pub purpose: ConsentPurpose,
    /// The version of the policy the subject agreed to.
    pub policy_version: String,
    /// When consent was given.
    pub granted_at: DateTime<Utc>,
    /// When consent lapses; `None` until revoked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// When the subject revoked it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
    /// What shows the grant, such as a signed receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<ConsentEvidence>,
}

impl ConsentRecord {
    /// A grant by `subject` for `purpose` under `policy_version`, made now, with a random id.
    pub fn new(
        subject: SubjectRef,
        purpose: ConsentPurpose,
        policy_version: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            subject,
            purpose,
            policy_version: policy_version.into(),
            granted_at: Utc::now(),
            expires_at: None,
            revoked_at: None,
            evidence: None,
        }
    }

    /// Sets the id, such as the id of the receipt that is the grant's evidence.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    /// Sets when consent was given.
    #[must_use]
    pub fn granted_at(mut self, at: DateTime<Utc>) -> Self {
        self.granted_at = at;
        self
    }

    /// Sets when consent lapses.
    #[must_use]
    pub fn with_expires_at(mut self, at: DateTime<Utc>) -> Self {
        self.expires_at = Some(at);
        self
    }

    /// Sets the evidence.
    #[must_use]
    pub fn with_evidence(mut self, evidence: ConsentEvidence) -> Self {
        self.evidence = Some(evidence);
        self
    }

    /// Whether the record is granted, unexpired, and unrevoked at `at`.
    pub fn is_live(&self, at: DateTime<Utc>) -> bool {
        self.granted_at <= at
            && self.expires_at.is_none_or(|expires| at < expires)
            && self.revoked_at.is_none_or(|revoked| at < revoked)
    }
}

/// The error a [`ConsentStore`] returns when a grant's id is taken.
pub fn consent_conflict_error(id: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Auth,
        ErrorCategory::Conflict,
        CONSENT_CONFLICT_CODE,
        format!("a consent record with id '{id}' already exists; give each grant its own id"),
    )
}

/// Consent records, by subject and purpose.
///
/// Consent gates read [`live`](Self::live) before a lasting write; revocation stops later
/// writes and erases nothing. Records are deployment-wide: one subject's consent covers
/// every application of the deployment.
///
/// [`InMemoryConsentStore`] is the reference implementation; `adk-session` ships the
/// durable ones. Every backend passes `adk_core::consent_store_conformance_tests!`
/// (feature `conformance`).
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::{ConsentPurpose, ConsentRecord, ConsentStore, InMemoryConsentStore};
/// use chrono::Utc;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let store = InMemoryConsentStore::new();
/// let visitor = SubjectRef::visitor("v:1");
/// store.grant(ConsentRecord::new(visitor.clone(), ConsentPurpose::Analytics, "2026-10")).await?;
///
/// let revoked = store.revoke_purpose(&visitor, Some(&ConsentPurpose::Analytics), Utc::now()).await?;
/// assert_eq!(revoked.len(), 1);
/// assert!(store.live(&visitor, &ConsentPurpose::Analytics, Utc::now()).await?.is_none());
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait ConsentStore: Send + Sync {
    /// Stores a grant.
    ///
    /// # Errors
    ///
    /// [`CONSENT_CONFLICT_CODE`] when a record with the same id exists.
    async fn grant(&self, record: ConsentRecord) -> Result<()>;

    /// Returns record `id`.
    async fn get(&self, id: &str) -> Result<Option<ConsentRecord>>;

    /// Returns every record of `subject`, oldest grant first.
    async fn for_subject(&self, subject: &SubjectRef) -> Result<Vec<ConsentRecord>>;

    /// Revokes record `id` at `at` and returns it; a revoked record keeps its first
    /// revocation time. `None` when no record has that id.
    async fn revoke(&self, id: &str, at: DateTime<Utc>) -> Result<Option<ConsentRecord>>;

    /// Erases every record of `subject` and returns how many.
    async fn erase(&self, subject: &SubjectRef) -> Result<u64>;

    /// Revokes every record of `subject` that is live at `at` for `purpose`, or for every
    /// purpose when `purpose` is `None`, and returns them as revoked.
    async fn revoke_purpose(
        &self,
        subject: &SubjectRef,
        purpose: Option<&ConsentPurpose>,
        at: DateTime<Utc>,
    ) -> Result<Vec<ConsentRecord>> {
        let mut revoked = Vec::new();
        for record in self.for_subject(subject).await? {
            if record.is_live(at) && purpose.is_none_or(|purpose| *purpose == record.purpose) {
                revoked.extend(self.revoke(&record.id, at).await?);
            }
        }
        Ok(revoked)
    }

    /// Returns the most recent record of `subject` live at `at` for `purpose`.
    async fn live(
        &self,
        subject: &SubjectRef,
        purpose: &ConsentPurpose,
        at: DateTime<Utc>,
    ) -> Result<Option<ConsentRecord>> {
        Ok(self
            .for_subject(subject)
            .await?
            .into_iter()
            .rev()
            .find(|record| record.purpose == *purpose && record.is_live(at)))
    }
}

/// In-memory [`ConsentStore`] for tests and single-process development.
///
/// Clones share the same records.
#[derive(Debug, Clone, Default)]
pub struct InMemoryConsentStore {
    records: Arc<tokio::sync::RwLock<HashMap<String, ConsentRecord>>>,
}

impl InMemoryConsentStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ConsentStore for InMemoryConsentStore {
    async fn grant(&self, record: ConsentRecord) -> Result<()> {
        let mut records = self.records.write().await;
        if records.contains_key(&record.id) {
            return Err(consent_conflict_error(&record.id));
        }
        records.insert(record.id.clone(), record);
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<ConsentRecord>> {
        Ok(self.records.read().await.get(id).cloned())
    }

    async fn for_subject(&self, subject: &SubjectRef) -> Result<Vec<ConsentRecord>> {
        let mut found: Vec<ConsentRecord> = self
            .records
            .read()
            .await
            .values()
            .filter(|record| record.subject == *subject)
            .cloned()
            .collect();
        found.sort_by(|a, b| a.granted_at.cmp(&b.granted_at).then_with(|| a.id.cmp(&b.id)));
        Ok(found)
    }

    async fn revoke(&self, id: &str, at: DateTime<Utc>) -> Result<Option<ConsentRecord>> {
        let mut records = self.records.write().await;
        Ok(records.get_mut(id).map(|record| {
            record.revoked_at.get_or_insert(at);
            record.clone()
        }))
    }

    async fn erase(&self, subject: &SubjectRef) -> Result<u64> {
        let mut records = self.records.write().await;
        let before = records.len();
        records.retain(|_, record| record.subject != *subject);
        Ok((before - records.len()) as u64)
    }
}
