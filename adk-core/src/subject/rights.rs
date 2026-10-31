//! Store adapters and the `SubjectRights` orchestrator.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::SubjectScope;
use super::consent::{ConsentPurpose, ConsentRecord, ConsentStore};
use super::registry::{SubjectRecord, SubjectRegistry, SubjectState, erasing_error};
use crate::audit_log::{
    AuditActor, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome, AuditTrail,
};
use crate::principal::SubjectRef;
use crate::{AdkError, Clock, ErrorCategory, ErrorComponent, Result, SystemClock};

/// The error code a [`SubjectDataStore`] returns from an operation it cannot perform.
pub const SUBJECT_UNSUPPORTED_CODE: &str = "subject.unsupported";

/// The error a [`SubjectDataStore`] returns from an operation it cannot perform, such as
/// exporting from a store with no way to list a user's records.
///
/// # Example
///
/// ```rust
/// use adk_core::subject::{SUBJECT_UNSUPPORTED_CODE, unsupported_error};
///
/// assert_eq!(unsupported_error("vertex-rag", "export").code, SUBJECT_UNSUPPORTED_CODE);
/// ```
pub fn unsupported_error(store: &str, operation: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Auth,
        ErrorCategory::Unsupported,
        SUBJECT_UNSUPPORTED_CODE,
        format!("the '{store}' store cannot {operation} by subject"),
    )
}

/// How a [`SubjectDataStore`] erases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
#[non_exhaustive]
pub enum StoreSupport {
    /// The data is gone when `erase` returns.
    Hard,
    /// `erase` starts a deletion that completes asynchronously, within `within`, such as
    /// a managed service's long-running delete.
    Eventual {
        /// The longest the deletion takes.
        #[serde(rename = "withinSecs", with = "duration_secs")]
        within: Duration,
    },
    /// The store holds the subject's data but cannot find or delete it by subject.
    Unsupported,
    /// The data is held outside the deployment's reach, such as by a model provider or an
    /// external telemetry collector; the report names the store so the deployment can act
    /// there.
    Unreachable,
}

mod duration_secs {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(value.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
        u64::deserialize(deserializer).map(Duration::from_secs)
    }
}

/// What one store's erasure did.
///
/// # Example
///
/// ```rust
/// use adk_core::subject::StoreErasure;
///
/// assert_eq!(StoreErasure::deleted(3), StoreErasure { deleted: 3, retained: 0, completes_by: None });
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreErasure {
    /// Items deleted.
    pub deleted: u64,
    /// Items the store keeps by design after erasure, such as noncurrent object versions
    /// under a retention policy. The report lists them; they are never silently counted
    /// as deleted.
    pub retained: u64,
    /// When an asynchronous deletion completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completes_by: Option<DateTime<Utc>>,
}

impl StoreErasure {
    /// `count` items deleted, none retained, done.
    pub fn deleted(count: u64) -> Self {
        Self { deleted: count, retained: 0, completes_by: None }
    }
}

/// One store family's adapter for subject rights: exports and erases what the store holds
/// about a [`SubjectScope`].
///
/// A store keyed by `(app, user)` looks the subject up under every
/// [user id](SubjectScope::user_ids) in every [application](SubjectScope::app_names) of the
/// scope. Every adapter passes `adk_core::subject_data_store_conformance_tests!` (feature
/// `conformance`): after a completed erasure no read path of the store returns the subject's
/// data, other subjects' data is untouched, and a repeat deletes nothing.
///
/// # Example
///
/// ```rust
/// use adk_core::subject::{StoreErasure, SubjectDataStore, SubjectScope};
/// use adk_core::{Result, async_trait};
/// use serde_json::{Value, json};
///
/// struct Newsletter;
///
/// #[async_trait]
/// impl SubjectDataStore for Newsletter {
///     fn name(&self) -> &str {
///         "newsletter"
///     }
///
///     async fn export(&self, scope: &SubjectScope) -> Result<Value> {
///         Ok(json!({ "subscriptions": [] }))
///     }
///
///     async fn erase(&self, scope: &SubjectScope) -> Result<StoreErasure> {
///         Ok(StoreErasure::deleted(0))
///     }
/// }
/// ```
#[async_trait]
pub trait SubjectDataStore: Send + Sync {
    /// The store's name in reports, such as `sessions` or `memory`.
    fn name(&self) -> &str;

    /// How the store erases. [`StoreSupport::Hard`] by default.
    fn support(&self) -> StoreSupport {
        StoreSupport::Hard
    }

    /// Every item the store holds about the scope, as JSON. An empty array, an empty
    /// object, or `null` means the store holds nothing.
    ///
    /// # Errors
    ///
    /// [`SUBJECT_UNSUPPORTED_CODE`] when the store cannot list by subject; any other error
    /// when reading fails.
    async fn export(&self, scope: &SubjectScope) -> Result<Value>;

    /// Erases every item the store holds about the scope.
    ///
    /// Called only when [`support`](Self::support) is `Hard` or `Eventual`. A repeat
    /// deletes nothing and succeeds.
    ///
    /// # Errors
    ///
    /// Returns an error when deleting fails; the subject stays
    /// [`Erasing`](SubjectState::Erasing) so a repeat can finish.
    async fn erase(&self, scope: &SubjectScope) -> Result<StoreErasure>;
}

/// What happened in one store during an erasure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "status")]
#[non_exhaustive]
pub enum StoreOutcome {
    /// The data is gone.
    #[serde(rename_all = "camelCase")]
    Erased {
        /// Items deleted.
        deleted: u64,
        /// Items kept by design.
        retained: u64,
    },
    /// The deletion was accepted and completes by `completes_by`.
    #[serde(rename_all = "camelCase")]
    Scheduled {
        /// Items whose deletion was accepted.
        deleted: u64,
        /// Items kept by design.
        retained: u64,
        /// When the deletion completes.
        completes_by: DateTime<Utc>,
    },
    /// The store cannot erase by subject; its data remains.
    Unsupported,
    /// The data is outside the deployment's reach.
    Unreachable,
    /// Deleting failed; a repeat retries it.
    #[serde(rename_all = "camelCase")]
    Failed {
        /// The error code.
        code: String,
        /// The error message.
        message: String,
    },
}

impl StoreOutcome {
    /// Whether the store's part of the erasure is done or under way.
    pub fn is_done(&self) -> bool {
        match self {
            Self::Erased { .. } | Self::Scheduled { .. } => true,
            Self::Unsupported | Self::Unreachable | Self::Failed { .. } => false,
        }
    }
}

/// One store's line in an [`ErasureReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreReport {
    /// The store's name.
    pub store: String,
    /// What happened there.
    #[serde(flatten)]
    pub outcome: StoreOutcome,
}

/// Whether an erasure reached every store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ErasureStatus {
    /// Every store erased the subject's data or accepted its deletion.
    Completed,
    /// At least one store is unsupported, unreachable, or failed; its data may remain.
    Partial,
}

/// The result of [`SubjectRights::erase`]. Names no subject, so it may be kept or logged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErasureReport {
    /// Whether every store was reached.
    pub status: ErasureStatus,
    /// How many subject ids were erased: the subject and its linked visitor ids.
    pub subjects: u64,
    /// One line per store, the consent store last.
    pub stores: Vec<StoreReport>,
    /// The latest time an asynchronous deletion completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completes_by: Option<DateTime<Utc>>,
    /// When the erasure ran.
    pub erased_at: DateTime<Utc>,
}

impl ErasureReport {
    /// Whether a store failed, so a repeat can finish the erasure. The subject stays
    /// [`Erasing`](SubjectState::Erasing) until one succeeds.
    pub fn failed(&self) -> bool {
        self.stores.iter().any(|line| matches!(line.outcome, StoreOutcome::Failed { .. }))
    }
}

/// What one store returned for an export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "status")]
#[non_exhaustive]
pub enum ExportOutcome {
    /// The store's data about the subject.
    Exported {
        /// The data.
        data: Value,
    },
    /// The store cannot list by subject.
    Unsupported,
    /// The data is outside the deployment's reach.
    Unreachable,
    /// Reading failed.
    Failed {
        /// The error code.
        code: String,
        /// The error message.
        message: String,
    },
}

/// One store's part of a [`SubjectExport`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreExport {
    /// The store's name.
    pub store: String,
    /// What it returned.
    #[serde(flatten)]
    pub outcome: ExportOutcome,
}

/// The result of [`SubjectRights::export`]: everything the deployment's registered stores
/// hold about a subject and its linked visitor ids.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubjectExport {
    /// The subject.
    pub subject: SubjectRef,
    /// Its linked visitor ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked: Vec<SubjectRef>,
    /// The registry's records of the subject and its linked ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub registry: Vec<SubjectRecord>,
    /// Every consent record of the subject and its linked ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consent: Vec<ConsentRecord>,
    /// One entry per store.
    pub stores: Vec<StoreExport>,
    /// When the export ran.
    pub exported_at: DateTime<Utc>,
}

impl SubjectExport {
    /// Whether every store returned its data.
    pub fn is_complete(&self) -> bool {
        self.stores.iter().all(|entry| matches!(entry.outcome, ExportOutcome::Exported { .. }))
    }
}

/// The result of [`SubjectRights::revoke`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Revocation {
    /// The records revoked.
    pub revoked: Vec<ConsentRecord>,
    /// The erasure that followed, when a revoked purpose erases the subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub erasure: Option<ErasureReport>,
}

/// Exports, erases, and links a subject's data across the registry, the consent store,
/// and every registered [`SubjectDataStore`].
///
/// Build one per deployment with the applications whose `(app, user)`-keyed stores it
/// covers. `link` trusts its arguments: the caller proves the visitor id (its token) and
/// a fresh authentication of the identified subject in the same request before calling it.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::{InMemoryConsentStore, InMemorySubjectRegistry, SubjectRights, VisitorId};
///
/// # async fn demo() -> adk_core::Result<()> {
/// let rights = SubjectRights::new(
///     Arc::new(InMemorySubjectRegistry::new()),
///     Arc::new(InMemoryConsentStore::new()),
/// )
/// .with_app("shop");
///
/// let visitor = VisitorId::generate().subject();
/// let alice = SubjectRef::human("alice");
/// rights.link(&visitor, &alice).await?;
///
/// // Export and erasure of alice cover the visitor id.
/// assert_eq!(rights.export(&alice).await?.linked, vec![visitor]);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct SubjectRights {
    registry: Arc<dyn SubjectRegistry>,
    consent: Arc<dyn ConsentStore>,
    stores: Vec<Arc<dyn SubjectDataStore>>,
    app_names: Vec<String>,
    erase_on_revoke: BTreeSet<ConsentPurpose>,
    audit: Option<AuditTrail>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for SubjectRights {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubjectRights")
            .field("stores", &self.stores.iter().map(|store| store.name()).collect::<Vec<_>>())
            .field("app_names", &self.app_names)
            .field("erase_on_revoke", &self.erase_on_revoke)
            .finish_non_exhaustive()
    }
}

impl SubjectRights {
    /// Rights over `registry` and `consent`, with no stores and no applications yet.
    pub fn new(registry: Arc<dyn SubjectRegistry>, consent: Arc<dyn ConsentStore>) -> Self {
        Self {
            registry,
            consent,
            stores: Vec::new(),
            app_names: Vec::new(),
            erase_on_revoke: BTreeSet::new(),
            audit: None,
            clock: Arc::new(SystemClock),
        }
    }

    /// Adds an application whose `(app, user)`-keyed stores are covered.
    #[must_use]
    pub fn with_app(mut self, app_name: impl Into<String>) -> Self {
        self.app_names.push(app_name.into());
        self
    }

    /// Adds a store.
    #[must_use]
    pub fn with_store(mut self, store: Arc<dyn SubjectDataStore>) -> Self {
        self.stores.push(store);
        self
    }

    /// Erases the subject when it revokes consent for `purpose`. Without this, revoking
    /// stops later writes and erases nothing.
    #[must_use]
    pub fn with_erase_on_revoke(mut self, purpose: ConsentPurpose) -> Self {
        self.erase_on_revoke.insert(purpose);
        self
    }

    /// Records links and erasures in `audit`, naming subjects by pseudonym.
    #[must_use]
    pub fn with_audit(mut self, audit: AuditTrail) -> Self {
        self.audit = Some(audit);
        self
    }

    /// Sets the clock that stamps revocations and reports.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The registry.
    pub fn registry(&self) -> &Arc<dyn SubjectRegistry> {
        &self.registry
    }

    /// The consent store.
    pub fn consent(&self) -> &Arc<dyn ConsentStore> {
        &self.consent
    }

    /// The scope of `subject`: it, its linked visitor ids, and the covered applications.
    ///
    /// # Errors
    ///
    /// Returns a registry error.
    pub async fn scope(&self, subject: &SubjectRef) -> Result<SubjectScope> {
        Ok(SubjectScope::new(self.app_names.iter().cloned(), subject.clone())
            .with_linked(self.registry.linked(subject).await?))
    }

    /// Fails with [`SUBJECT_ERASING_CODE`](super::SUBJECT_ERASING_CODE) when `subject` is
    /// being erased. Consent gates call it before a lasting write.
    ///
    /// # Errors
    ///
    /// `subject.erasing`, or a registry error.
    pub async fn ensure_writable(&self, subject: &SubjectRef) -> Result<()> {
        match self.registry.get(subject).await? {
            Some(record) if record.state == SubjectState::Erasing => Err(erasing_error(subject)),
            Some(_) | None => Ok(()),
        }
    }

    /// Records a grant of consent.
    ///
    /// # Errors
    ///
    /// `subject.erasing` while the subject is being erased; `consent.conflict` when the
    /// record's id is taken; or a store error.
    pub async fn grant(&self, record: ConsentRecord) -> Result<()> {
        self.ensure_writable(&record.subject).await?;
        self.consent.grant(record).await
    }

    /// Revokes the subject's live consent for `purpose`, or for every purpose when `None`.
    /// When a revoked purpose is one passed to
    /// [`with_erase_on_revoke`](Self::with_erase_on_revoke), the subject is erased as well.
    ///
    /// # Errors
    ///
    /// Returns a store error.
    pub async fn revoke(
        &self,
        subject: &SubjectRef,
        purpose: Option<&ConsentPurpose>,
    ) -> Result<Revocation> {
        let revoked = self.consent.revoke_purpose(subject, purpose, self.clock.now()).await?;
        let erase = revoked.iter().any(|record| self.erase_on_revoke.contains(&record.purpose));
        let erasure = if erase { Some(self.erase(subject).await?) } else { None };
        Ok(Revocation { revoked, erasure })
    }

    /// Links the visitor id `visitor` to the identified subject `identified`: export and
    /// erasure of `identified` cover `visitor` from then on. Data stays where it is.
    ///
    /// The caller has proved both in one request — the visitor's token and a fresh
    /// authentication of `identified` — and rotates the visitor's token afterwards.
    ///
    /// # Errors
    ///
    /// `subject.link.invalid`, `subject.link.consumed`, or `subject.erasing`, as
    /// [`SubjectRegistry::link`].
    pub async fn link(
        &self,
        visitor: &SubjectRef,
        identified: &SubjectRef,
    ) -> Result<SubjectRecord> {
        let record = self.registry.link(visitor, identified).await?;
        if let Some(trail) = &self.audit {
            let identified = self.registry.ensure(identified).await?;
            let draft = AuditRecordDraft::new(AuditRecordKind::Authentication, "subject.link")
                .actor(AuditActor::subject(SubjectRef::new(
                    identified.subject.kind,
                    identified.pseudonym,
                )))
                .outcome(AuditRecordOutcome::Succeeded)
                .ext("visitor", Value::String(record.pseudonym.clone()));
            trail.record(draft).await?;
        }
        Ok(record)
    }

    /// Everything the registered stores hold about `subject` and its linked visitor ids.
    ///
    /// A store that cannot export is reported, never left out.
    ///
    /// # Errors
    ///
    /// Returns a registry or consent store error; store errors are reported per store.
    pub async fn export(&self, subject: &SubjectRef) -> Result<SubjectExport> {
        let scope = self.scope(subject).await?;
        let mut registry = Vec::new();
        let mut consent = Vec::new();
        for each in scope.subjects() {
            registry.extend(self.registry.get(each).await?);
            consent.extend(self.consent.for_subject(each).await?);
        }
        let mut stores = Vec::with_capacity(self.stores.len());
        for store in &self.stores {
            let outcome = match store.support() {
                StoreSupport::Unreachable => ExportOutcome::Unreachable,
                StoreSupport::Hard | StoreSupport::Eventual { .. } | StoreSupport::Unsupported => {
                    match store.export(&scope).await {
                        Ok(data) => ExportOutcome::Exported { data },
                        Err(error) if error.code == SUBJECT_UNSUPPORTED_CODE => {
                            ExportOutcome::Unsupported
                        }
                        Err(error) => {
                            tracing::warn!(subject.store = store.name(), error = %error, "subject export failed");
                            ExportOutcome::Failed {
                                code: error.code.to_string(),
                                message: error.message,
                            }
                        }
                    }
                }
            };
            stores.push(StoreExport { store: store.name().to_owned(), outcome });
        }
        Ok(SubjectExport {
            subject: scope.subject,
            linked: scope.linked,
            registry,
            consent,
            stores,
            exported_at: self.clock.now(),
        })
    }

    /// Erases everything the registered stores hold about `subject` and its linked visitor
    /// ids, in the order the [module documentation](super) gives.
    ///
    /// A repeat is safe: a completed erasure leaves nothing to find, and one with a failed
    /// store resumes where it stopped.
    ///
    /// # Errors
    ///
    /// Returns a registry error or a consent store error from revocation; errors of
    /// individual stores are reported per store.
    pub async fn erase(&self, subject: &SubjectRef) -> Result<ErasureReport> {
        let now = self.clock.now();
        let pseudonym = self.registry.set_state(subject, SubjectState::Erasing).await?.pseudonym;
        let scope = self.scope(subject).await?;
        for linked in &scope.linked {
            self.registry.set_state(linked, SubjectState::Erasing).await?;
        }
        for each in scope.subjects() {
            self.consent.revoke_purpose(each, None, now).await?;
        }

        let mut lines = Vec::with_capacity(self.stores.len() + 1);
        for store in &self.stores {
            let outcome = match store.support() {
                StoreSupport::Unsupported => StoreOutcome::Unsupported,
                StoreSupport::Unreachable => StoreOutcome::Unreachable,
                support @ (StoreSupport::Hard | StoreSupport::Eventual { .. }) => {
                    match store.erase(&scope).await {
                        Ok(erasure) => {
                            let eventual = match support {
                                StoreSupport::Eventual { within } => {
                                    chrono::Duration::from_std(within)
                                        .ok()
                                        .map(|within| now + within)
                                }
                                StoreSupport::Hard
                                | StoreSupport::Unsupported
                                | StoreSupport::Unreachable => None,
                            };
                            match erasure.completes_by.or(eventual) {
                                Some(completes_by) => StoreOutcome::Scheduled {
                                    deleted: erasure.deleted,
                                    retained: erasure.retained,
                                    completes_by,
                                },
                                None => StoreOutcome::Erased {
                                    deleted: erasure.deleted,
                                    retained: erasure.retained,
                                },
                            }
                        }
                        Err(error) => {
                            tracing::error!(subject.store = store.name(), error = %error, "subject erasure failed in a store");
                            StoreOutcome::Failed {
                                code: error.code.to_string(),
                                message: error.message,
                            }
                        }
                    }
                }
            };
            lines.push(StoreReport { store: store.name().to_owned(), outcome });
        }

        let mut consent_erased = Ok(0);
        for each in scope.subjects() {
            consent_erased = match (consent_erased, self.consent.erase(each).await) {
                (Ok(total), Ok(count)) => Ok(total + count),
                (Err(error), _) | (Ok(_), Err(error)) => Err(error),
            };
        }
        lines.push(StoreReport {
            store: "consent".to_owned(),
            outcome: match consent_erased {
                Ok(deleted) => StoreOutcome::Erased { deleted, retained: 0 },
                Err(error) => {
                    StoreOutcome::Failed { code: error.code.to_string(), message: error.message }
                }
            },
        });

        let report = ErasureReport {
            status: if lines.iter().all(|line| line.outcome.is_done()) {
                ErasureStatus::Completed
            } else {
                ErasureStatus::Partial
            },
            subjects: scope.subjects().count() as u64,
            completes_by: lines
                .iter()
                .filter_map(|line| match line.outcome {
                    StoreOutcome::Scheduled { completes_by, .. } => Some(completes_by),
                    StoreOutcome::Erased { .. }
                    | StoreOutcome::Unsupported
                    | StoreOutcome::Unreachable
                    | StoreOutcome::Failed { .. } => None,
                })
                .max(),
            stores: lines,
            erased_at: now,
        };

        if let Some(trail) = &self.audit {
            let draft = AuditRecordDraft::new(AuditRecordKind::Erasure, "subject.erase")
                .actor(AuditActor::subject(SubjectRef::new(subject.kind, pseudonym)))
                .outcome(if report.failed() {
                    AuditRecordOutcome::Failed
                } else {
                    AuditRecordOutcome::Succeeded
                })
                .ext("subjects", Value::from(report.subjects))
                .ext("stores", erasure_audit_lines(&report.stores));
            if let Err(error) = trail.record(draft).await {
                tracing::warn!(error = %error, "subject erasure ran but its audit record was not written");
            }
        }

        if !report.failed() {
            for each in scope.subjects() {
                self.registry.remove(each).await?;
            }
        }
        Ok(report)
    }
}

/// The report's store lines without error messages, which may name the subject.
fn erasure_audit_lines(lines: &[StoreReport]) -> Value {
    Value::Array(
        lines
            .iter()
            .map(|line| {
                let mut value = json!(line);
                if let Some(fields) = value.as_object_mut() {
                    fields.remove("message");
                }
                value
            })
            .collect(),
    )
}
