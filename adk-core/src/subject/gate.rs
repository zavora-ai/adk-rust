//! The consent gate: whether a subject may have something lasting written for a purpose.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::VisitorId;
use super::consent::{ConsentPurpose, ConsentStore};
use super::registry::{SubjectRegistry, SubjectState, erasing_error};
use crate::principal::{SubjectKind, SubjectRef};
use crate::{AdkError, Clock, ErrorCategory, ErrorComponent, Result, SystemClock};

/// The error code of a write or tool call refused because its subject has no live consent
/// for the purpose it serves.
pub const CONSENT_REQUIRED_CODE: &str = "consent.required";

/// The error returned when `purpose` has no live consent.
///
/// # Example
///
/// ```rust
/// use adk_core::subject::{CONSENT_REQUIRED_CODE, ConsentPurpose, consent_required_error};
///
/// let error = consent_required_error(&ConsentPurpose::PersistentProfile);
/// assert_eq!(error.code, CONSENT_REQUIRED_CODE);
/// ```
pub fn consent_required_error(purpose: &ConsentPurpose) -> AdkError {
    AdkError::new(
        ErrorComponent::Auth,
        ErrorCategory::Forbidden,
        CONSENT_REQUIRED_CODE,
        format!(
            "this needs the subject's consent for '{purpose}', which it has not given or has \
             revoked; ask for consent before writing anything lasting for this purpose"
        ),
    )
}

/// Decides whether a subject's consent allows a lasting write, and refuses writes for a
/// subject that is being erased.
///
/// Gated subjects are visitors by default — a user id that parses as a [`VisitorId`] —
/// and, after [`for_every_subject`](Self::for_every_subject), humans and services too.
/// Agents are never gated. A subject that is not gated is always permitted.
///
/// The gate is read by:
///
/// | Reader | Purpose | Without consent |
/// |--------|---------|-----------------|
/// | `adk_session::ConsentGatedSessionService` | [`SessionContinuity`](ConsentPurpose::SessionContinuity) | The session lives in process memory only |
/// | `adk_session::ConsentGatedSessionService` | [`PersistentProfile`](ConsentPurpose::PersistentProfile) | `user:` keys are dropped from the write |
/// | `adk_memory::ConsentGatedMemoryService` | [`PersistentProfile`](ConsentPurpose::PersistentProfile) | Nothing is added; search returns nothing |
/// | [`authorize_tool_call`](crate::authorize_tool_call), through [`RunConfig::consent_gate`](crate::RunConfig::consent_gate) | The tool's purpose from [`with_tool`](Self::with_tool) | The call is refused with [`CONSENT_REQUIRED_CODE`] |
/// | The runner | [`Analytics`](ConsentPurpose::Analytics) | The run's spans carry no user id |
///
/// With a [registry](Self::with_registry), every writer above and the runner refuse a
/// subject that is being erased.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::subject::{
///     ConsentGate, ConsentPurpose, ConsentRecord, ConsentStore, InMemoryConsentStore, VisitorId,
/// };
///
/// # async fn demo() -> adk_core::Result<()> {
/// let consent = Arc::new(InMemoryConsentStore::new());
/// let gate = ConsentGate::new(consent.clone()).with_tool("subscribe", ConsentPurpose::Newsletter);
///
/// let visitor = VisitorId::generate();
/// let subject = gate.subject_of(visitor.as_str()).expect("visitors are gated");
/// assert!(!gate.permits(&subject, &ConsentPurpose::Newsletter).await?);
///
/// consent.grant(ConsentRecord::new(subject.clone(), ConsentPurpose::Newsletter, "2026-10")).await?;
/// gate.require(&subject, &ConsentPurpose::Newsletter).await?;
/// assert_eq!(gate.subject_of("alice"), None, "humans are not gated by default");
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct ConsentGate {
    consent: Arc<dyn ConsentStore>,
    registry: Option<Arc<dyn SubjectRegistry>>,
    every_subject: bool,
    tools: BTreeMap<String, ConsentPurpose>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for ConsentGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsentGate")
            .field("every_subject", &self.every_subject)
            .field("tools", &self.tools)
            .finish_non_exhaustive()
    }
}

impl ConsentGate {
    /// A gate over `consent` for visitors.
    pub fn new(consent: Arc<dyn ConsentStore>) -> Self {
        Self {
            consent,
            registry: None,
            every_subject: false,
            tools: BTreeMap::new(),
            clock: Arc::new(SystemClock),
        }
    }

    /// Refuses writes for a subject `registry` records as being erased.
    #[must_use]
    pub fn with_registry(mut self, registry: Arc<dyn SubjectRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Gates humans and services as well as visitors.
    #[must_use]
    pub fn for_every_subject(mut self) -> Self {
        self.every_subject = true;
        self
    }

    /// Requires consent for `purpose` before the tool `tool_name` runs.
    #[must_use]
    pub fn with_tool(mut self, tool_name: impl Into<String>, purpose: ConsentPurpose) -> Self {
        self.tools.insert(tool_name.into(), purpose);
        self
    }

    /// Sets the clock consent is checked against.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The purpose the tool `tool_name` needs consent for, if any.
    pub fn tool_purpose(&self, tool_name: &str) -> Option<&ConsentPurpose> {
        self.tools.get(tool_name)
    }

    /// The gated subject a session or memory user id names: a visitor when the id is a
    /// [`VisitorId`]; otherwise a human when the gate covers every subject; otherwise
    /// `None`.
    pub fn subject_of(&self, user_id: &str) -> Option<SubjectRef> {
        if user_id.parse::<VisitorId>().is_ok() {
            Some(SubjectRef::visitor(user_id))
        } else if self.every_subject {
            Some(SubjectRef::human(user_id))
        } else {
            None
        }
    }

    /// Whether writes for `subject` need its consent.
    pub fn is_gated(&self, subject: &SubjectRef) -> bool {
        match subject.kind {
            SubjectKind::Visitor => true,
            SubjectKind::Human | SubjectKind::Service => self.every_subject,
            SubjectKind::Agent => false,
        }
    }

    /// Fails with `subject.erasing` when the registry records `subject` as being erased.
    ///
    /// # Errors
    ///
    /// `subject.erasing`, or a registry error.
    pub async fn ensure_writable(&self, subject: &SubjectRef) -> Result<()> {
        let Some(registry) = &self.registry else { return Ok(()) };
        match registry.get(subject).await? {
            Some(record) if record.state == SubjectState::Erasing => Err(erasing_error(subject)),
            Some(_) | None => Ok(()),
        }
    }

    /// Whether `subject` may have something lasting written for `purpose`: always for a
    /// subject that is not gated, otherwise only with live consent.
    ///
    /// # Errors
    ///
    /// Returns a consent store error.
    pub async fn permits(&self, subject: &SubjectRef, purpose: &ConsentPurpose) -> Result<bool> {
        if !self.is_gated(subject) {
            return Ok(true);
        }
        Ok(self.consent.live(subject, purpose, self.clock.now()).await?.is_some())
    }

    /// Fails unless `subject` is writable and [`permits`](Self::permits) `purpose`.
    ///
    /// # Errors
    ///
    /// `subject.erasing`, [`CONSENT_REQUIRED_CODE`], or a store error.
    pub async fn require(&self, subject: &SubjectRef, purpose: &ConsentPurpose) -> Result<()> {
        self.ensure_writable(subject).await?;
        if self.permits(subject, purpose).await? {
            Ok(())
        } else {
            Err(consent_required_error(purpose))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subject::{
        ConsentRecord, InMemoryConsentStore, InMemorySubjectRegistry, SUBJECT_ERASING_CODE,
    };

    #[tokio::test]
    async fn gates_visitors_by_default_and_everyone_on_request() {
        let consent = Arc::new(InMemoryConsentStore::new());
        let registry = Arc::new(InMemorySubjectRegistry::new());
        let gate = ConsentGate::new(consent.clone()).with_registry(registry.clone());
        let visitor = VisitorId::generate();
        let subject = gate.subject_of(visitor.as_str()).unwrap();
        assert_eq!(subject, visitor.subject());
        assert_eq!(gate.subject_of("alice"), None);
        assert!(
            gate.permits(&SubjectRef::human("alice"), &ConsentPurpose::Analytics).await.unwrap()
        );

        let purpose = ConsentPurpose::PersistentProfile;
        assert_eq!(gate.require(&subject, &purpose).await.unwrap_err().code, CONSENT_REQUIRED_CODE);
        consent.grant(ConsentRecord::new(subject.clone(), purpose.clone(), "v1")).await.unwrap();
        gate.require(&subject, &purpose).await.unwrap();

        registry.set_state(&subject, SubjectState::Erasing).await.unwrap();
        assert_eq!(gate.require(&subject, &purpose).await.unwrap_err().code, SUBJECT_ERASING_CODE);

        let everyone = ConsentGate::new(consent).for_every_subject();
        assert_eq!(everyone.subject_of("alice"), Some(SubjectRef::human("alice")));
        assert!(!everyone.permits(&SubjectRef::service("crm"), &purpose).await.unwrap());
        assert!(!everyone.is_gated(&SubjectRef::new(SubjectKind::Agent, "urn:adk:agent:a")));
    }
}
