//! Audit records that name subjects by pseudonym, and the audit log as a subject data store.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::SubjectScope;
use super::registry::SubjectRegistry;
use super::rights::{StoreErasure, SubjectDataStore};
use crate::Result;
use crate::audit_log::{AuditActor, AuditLogStore, AuditRecord, AuditRecordDraft, AuditRecorder};
use crate::principal::{SubjectKind, SubjectRef};

/// An [`AuditRecorder`] that replaces the subjects a record names with their registry
/// pseudonyms before the record reaches the recorder it wraps.
///
/// `actor.onBehalfOf` and every subject in `actor.delegationChain` become
/// `{kind, id: "psn:…"}` for each subject the recorder covers: visitors by default, humans
/// and services too after [`for_every_subject`](Self::for_every_subject). Agents keep their
/// ids. Erasing the subject removes the pseudonym from the registry, so the records stay in
/// the log, verify, and no longer lead back to the person.
///
/// A pseudonym that cannot be resolved fails the record, so the trail's
/// [`AuditFailureMode`](crate::audit_log::AuditFailureMode) decides whether the action
/// proceeds.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditRecorder, InMemoryAuditRecorder};
/// use adk_core::subject::{InMemorySubjectRegistry, PseudonymousRecorder, VisitorId};
///
/// # async fn demo() -> adk_core::Result<()> {
/// let inner = Arc::new(InMemoryAuditRecorder::new());
/// let recorder = PseudonymousRecorder::new(inner.clone(), Arc::new(InMemorySubjectRegistry::new()));
/// let visitor = VisitorId::generate();
/// recorder
///     .record(AuditRecordDraft::new(AuditRecordKind::ToolCall, "search").actor(AuditActor::subject(visitor.subject())))
///     .await?;
/// let named = inner.records()[0].actor.on_behalf_of.clone().unwrap();
/// assert!(named.id.starts_with("psn:"));
/// # Ok(())
/// # }
/// ```
pub struct PseudonymousRecorder {
    inner: Arc<dyn AuditRecorder>,
    registry: Arc<dyn SubjectRegistry>,
    every_subject: bool,
}

impl PseudonymousRecorder {
    /// Records through `inner`, naming visitors by their pseudonyms in `registry`.
    pub fn new(inner: Arc<dyn AuditRecorder>, registry: Arc<dyn SubjectRegistry>) -> Self {
        Self { inner, registry, every_subject: false }
    }

    /// Names humans and services by pseudonym as well.
    #[must_use]
    pub fn for_every_subject(mut self) -> Self {
        self.every_subject = true;
        self
    }

    /// `subject` as the record names it: its pseudonym when covered, else unchanged.
    async fn named(&self, subject: SubjectRef) -> Result<SubjectRef> {
        let covered = match subject.kind {
            SubjectKind::Visitor => true,
            SubjectKind::Human | SubjectKind::Service => self.every_subject,
            SubjectKind::Agent => false,
        };
        if !covered {
            return Ok(subject);
        }
        let pseudonym = self.registry.ensure(&subject).await?.pseudonym;
        Ok(SubjectRef::new(subject.kind, pseudonym))
    }
}

impl std::fmt::Debug for PseudonymousRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PseudonymousRecorder")
            .field("every_subject", &self.every_subject)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl AuditRecorder for PseudonymousRecorder {
    fn digest(&self, value: &Value) -> String {
        self.inner.digest(value)
    }

    async fn record(&self, mut draft: AuditRecordDraft) -> Result<()> {
        let AuditActor { on_behalf_of, delegation_chain, .. } = &mut draft.actor;
        if let Some(subject) = on_behalf_of.take() {
            *on_behalf_of = Some(self.named(subject).await?);
        }
        let mut chain = Vec::with_capacity(delegation_chain.len());
        for subject in delegation_chain.drain(..) {
            chain.push(self.named(subject).await?);
        }
        *delegation_chain = chain;
        self.inner.record(draft).await
    }
}

/// Records read per page while scanning a log.
const SCAN_PAGE: u64 = 1_000;

/// The audit log as a [`SubjectDataStore`]: exports the records whose actor names a
/// subject of the scope by id, and erases them by redacting their bodies.
///
/// A record names the subject when its `onBehalfOf` or `delegationChain` holds the
/// subject's id as any kind but an agent: a run a scheduler started for `alice` acts for
/// `service:alice`, and that record is hers too.
///
/// Redaction keeps every leaf hash ([`AuditLogStore::redact`]), so checkpoints and proofs
/// still verify; leaves are never removed. Records written through a
/// [`PseudonymousRecorder`] name a pseudonym instead and are left as they are: the
/// registry forgets the pseudonym when the subject is erased. The store covers every log
/// of every application, since audit logs are kept per replica, not per application.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::audit_log::InMemoryAuditLogStore;
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::{AuditLogSubjectData, SubjectDataStore, SubjectScope};
///
/// # async fn demo() -> adk_core::Result<()> {
/// let audit = AuditLogSubjectData::new(Arc::new(InMemoryAuditLogStore::new()));
/// let erased = audit.erase(&SubjectScope::new(["shop"], SubjectRef::human("alice"))).await?;
/// assert_eq!(erased.deleted, 0);
/// # Ok(())
/// # }
/// ```
pub struct AuditLogSubjectData {
    store: Arc<dyn AuditLogStore>,
}

impl AuditLogSubjectData {
    /// The adapter over `store`.
    pub fn new(store: Arc<dyn AuditLogStore>) -> Self {
        Self { store }
    }

    /// The records of every log whose actor names a subject of `scope`, by log.
    async fn naming(&self, scope: &SubjectScope) -> Result<BTreeMap<String, Vec<AuditRecord>>> {
        let names = |record: &AuditRecord| {
            let actor = &record.actor;
            actor
                .on_behalf_of
                .iter()
                .chain(actor.delegation_chain.iter())
                .filter(|named| named.kind != SubjectKind::Agent)
                .any(|named| scope.subjects().any(|subject| subject.id == named.id))
        };
        let mut found = BTreeMap::new();
        for log_id in self.store.logs().await? {
            let size = self.store.size(&log_id).await?;
            let mut records = Vec::new();
            let mut start = 0;
            while start < size {
                let end = (start + SCAN_PAGE).min(size);
                for entry in self.store.read(&log_id, start..end).await? {
                    let Some(body) = entry.record else { continue };
                    // A body that does not parse as a record names no subject this adapter can see.
                    if let Ok(record) = serde_json::from_str::<AuditRecord>(&body)
                        && names(&record)
                    {
                        records.push(record);
                    }
                }
                start = end;
            }
            if !records.is_empty() {
                found.insert(log_id, records);
            }
        }
        Ok(found)
    }
}

impl std::fmt::Debug for AuditLogSubjectData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditLogSubjectData").finish_non_exhaustive()
    }
}

#[async_trait]
impl SubjectDataStore for AuditLogSubjectData {
    fn name(&self) -> &str {
        "audit_log"
    }

    async fn export(&self, scope: &SubjectScope) -> Result<Value> {
        let logs: Vec<Value> = self
            .naming(scope)
            .await?
            .into_iter()
            .map(|(log_id, records)| json!({ "logId": log_id, "records": records }))
            .collect();
        Ok(json!({ "logs": logs }))
    }

    async fn erase(&self, scope: &SubjectScope) -> Result<StoreErasure> {
        let mut deleted = 0;
        for (log_id, records) in self.naming(scope).await? {
            let seqs: Vec<u64> = records.iter().map(|record| record.seq).collect();
            deleted += self.store.redact(&log_id, &seqs).await?;
        }
        Ok(StoreErasure::deleted(deleted))
    }
}
