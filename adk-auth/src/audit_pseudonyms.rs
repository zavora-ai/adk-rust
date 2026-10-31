//! An audit sink that names people by pseudonym and keeps no client addresses.

use std::sync::Arc;

use adk_core::principal::{SubjectKind, SubjectRef};
use adk_core::subject::{SubjectRegistry, VisitorId};
use chrono::{DateTime, Utc};

use crate::AuthError;
use crate::audit::{AuditEvent, AuditFilter, AuditSink};

/// An [`AuditSink`] that rewrites each event before the sink it wraps stores it: the user
/// and `on_behalf_of` become registry pseudonyms (`psn:…`), and the client address is
/// dropped.
///
/// It covers visitors by default — a `user` that parses as a [`VisitorId`], and an
/// `on_behalf_of` of kind [`Visitor`](SubjectKind::Visitor) — and every human and service
/// after [`for_every_subject`](Self::for_every_subject). The rewrite happens before the
/// wrapped sink computes its hash chain, so the chain stays valid; erasing a subject
/// removes its pseudonym from the registry, so stored events no longer lead back to it.
/// Free-form `metadata` is stored as given.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_auth::{AuditEvent, AuditFilter, AuditOutcome, AuditSink, InMemoryAuditSink, PseudonymousAuditSink};
/// use adk_core::subject::{InMemorySubjectRegistry, VisitorId};
///
/// # async fn demo() -> Result<(), adk_auth::AuthError> {
/// let inner = Arc::new(InMemoryAuditSink::new());
/// let sink = PseudonymousAuditSink::new(inner.clone(), Arc::new(InMemorySubjectRegistry::new()));
/// let visitor = VisitorId::generate();
/// sink.log(
///     AuditEvent::tool_access(visitor.as_str(), "search", AuditOutcome::Allowed)
///         .with_ip_address("203.0.113.7"),
/// )
/// .await?;
/// let stored = &inner.query(&AuditFilter::default()).await?[0];
/// assert!(stored.user.starts_with("psn:"));
/// assert!(stored.ip_address.is_none());
/// # Ok(())
/// # }
/// ```
pub struct PseudonymousAuditSink {
    inner: Arc<dyn AuditSink>,
    registry: Arc<dyn SubjectRegistry>,
    every_subject: bool,
}

impl PseudonymousAuditSink {
    /// Stores events in `inner`, naming visitors by their pseudonyms in `registry`.
    pub fn new(inner: Arc<dyn AuditSink>, registry: Arc<dyn SubjectRegistry>) -> Self {
        Self { inner, registry, every_subject: false }
    }

    /// Names humans and services by pseudonym as well.
    #[must_use]
    pub fn for_every_subject(mut self) -> Self {
        self.every_subject = true;
        self
    }

    async fn pseudonym(&self, subject: &SubjectRef) -> Result<String, AuthError> {
        self.registry
            .ensure(subject)
            .await
            .map(|record| record.pseudonym)
            .map_err(|error| AuthError::AuditError(format!("pseudonym not resolved: {error}")))
    }

    fn covers(&self, kind: SubjectKind) -> bool {
        match kind {
            SubjectKind::Visitor => true,
            SubjectKind::Human | SubjectKind::Service => self.every_subject,
            _ => false,
        }
    }

    async fn rewrite(&self, mut event: AuditEvent) -> Result<AuditEvent, AuthError> {
        event.ip_address = None;
        let user = if event.user.parse::<VisitorId>().is_ok() {
            SubjectRef::visitor(event.user.clone())
        } else {
            SubjectRef::human(event.user.clone())
        };
        if !event.user.is_empty() && self.covers(user.kind) {
            event.user = self.pseudonym(&user).await?;
        }
        if let Some(subject) = event.on_behalf_of.take() {
            event.on_behalf_of = Some(if self.covers(subject.kind) {
                SubjectRef::new(subject.kind, self.pseudonym(&subject).await?)
            } else {
                subject
            });
        }
        Ok(event)
    }
}

impl std::fmt::Debug for PseudonymousAuditSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PseudonymousAuditSink")
            .field("every_subject", &self.every_subject)
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl AuditSink for PseudonymousAuditSink {
    async fn log(&self, event: AuditEvent) -> Result<(), AuthError> {
        self.inner.log(self.rewrite(event).await?).await
    }

    async fn log_batch(&self, events: Vec<AuditEvent>) -> Result<(), AuthError> {
        let mut rewritten = Vec::with_capacity(events.len());
        for event in events {
            rewritten.push(self.rewrite(event).await?);
        }
        self.inner.log_batch(rewritten).await
    }

    async fn query(&self, filter: &AuditFilter) -> Result<Vec<AuditEvent>, AuthError> {
        self.inner.query(filter).await
    }

    async fn purge_before(&self, cutoff: DateTime<Utc>) -> Result<u64, AuthError> {
        self.inner.purge_before(cutoff).await
    }

    async fn flush(&self) -> Result<(), AuthError> {
        self.inner.flush().await
    }
}
