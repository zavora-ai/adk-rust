//! [`VisionCx`]: what every vision call carries.

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditTrail};
use adk_core::{ActingContext, GovernanceControl, ModelCallScope};
use tokio_util::sync::CancellationToken;

use crate::error::{Result, VisionError};
use crate::media::MediaScope;

/// What every vision call carries: cancellation, the metering scope (budget, spend ledger
/// and kill switches), the acting agent, the media scope new images are stored under, the
/// audit trail `vision.*` records go to, and the tracing span.
///
/// # Example
///
/// ```
/// use adk_core::GovernanceControl;
/// use adk_vision::VisionCx;
///
/// let control = GovernanceControl::new();
/// let cx = VisionCx::new().with_governance(control.clone());
/// assert!(cx.check().is_ok());
/// control.freeze("incident 42");
/// assert!(cx.check().is_err());
/// ```
#[derive(Clone, Debug)]
pub struct VisionCx {
    cancel: CancellationToken,
    scope: ModelCallScope,
    acting: Option<ActingContext>,
    media: Option<MediaScope>,
    audit: Option<VisionAudit>,
    span: tracing::Span,
}

/// The audit trail of the run a vision call belongs to, and who acts in it.
#[derive(Clone)]
struct VisionAudit {
    trail: AuditTrail,
    actor: AuditActor,
}

impl std::fmt::Debug for VisionAudit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VisionAudit").field("actor", &self.actor).finish_non_exhaustive()
    }
}

impl Default for VisionCx {
    fn default() -> Self {
        Self::new()
    }
}

impl VisionCx {
    /// A context that meters nothing and is never cancelled.
    pub fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            scope: ModelCallScope::new(),
            acting: None,
            media: None,
            audit: None,
            span: tracing::Span::current(),
        }
    }

    /// A context under the metering scope made current by an enclosing
    /// [`ModelCallScope::within`], such as the one agents set around every tool call.
    pub fn current() -> Self {
        Self { scope: ModelCallScope::current().unwrap_or_default(), ..Self::new() }
    }

    /// Meters calls under `scope`: its budget, spend ledger and kill switches.
    pub fn with_scope(mut self, scope: ModelCallScope) -> Self {
        self.scope = scope;
        self
    }

    /// Adds a kill switch: while it is frozen, every call and every release is refused
    /// (V-I17).
    pub fn with_governance(mut self, control: GovernanceControl) -> Self {
        self.scope = self.scope.with_governance(control);
        self
    }

    /// Cancels calls when `token` fires.
    pub fn with_cancellation(mut self, token: CancellationToken) -> Self {
        self.cancel = token;
        self
    }

    /// Names the acting agent.
    pub fn with_acting(mut self, acting: ActingContext) -> Self {
        self.acting = Some(acting);
        self
    }

    /// Stores new images (edits, egress copies) under `scope`.
    pub fn with_media_scope(mut self, scope: MediaScope) -> Self {
        self.media = Some(scope);
        self
    }

    /// Writes the call's `vision.*` audit records — egress releases and refusals, perception
    /// calls and edits (spec 3.7 §9.3) — to `trail`, with `actor` as the acting agent. The
    /// trail is the run's (`RunConfig::audit`), signed when the run's recorder signs.
    pub fn with_audit(mut self, trail: AuditTrail, actor: AuditActor) -> Self {
        self.audit = Some(VisionAudit { trail, actor });
        self
    }

    /// Records the call's spans under `span`.
    pub fn with_span(mut self, span: tracing::Span) -> Self {
        self.span = span;
        self
    }

    /// The cancellation token.
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancel
    }

    /// The metering scope.
    pub fn scope(&self) -> &ModelCallScope {
        &self.scope
    }

    /// The acting agent, when one is named.
    pub fn acting(&self) -> Option<&ActingContext> {
        self.acting.as_ref()
    }

    /// The media scope, when one is set.
    pub fn media_scope(&self) -> Option<&MediaScope> {
        self.media.as_ref()
    }

    /// The audit trail `vision.*` records go to, when one is set.
    pub fn audit_trail(&self) -> Option<&AuditTrail> {
        self.audit.as_ref().map(|audit| &audit.trail)
    }

    /// Writes `draft` to the audit trail as the acting agent; does nothing without a trail.
    ///
    /// # Errors
    ///
    /// The trail's error, when its failure mode fails closed.
    pub(crate) async fn audit(&self, draft: AuditRecordDraft) -> Result<()> {
        if let Some(audit) = &self.audit {
            audit.trail.record(draft.actor(audit.actor.clone())).await?;
        }
        Ok(())
    }

    /// The tracing span.
    pub fn span(&self) -> &tracing::Span {
        &self.span
    }

    /// Refuses when the call is cancelled or a kill switch is frozen.
    ///
    /// # Errors
    ///
    /// [`VisionError::Cancelled`], or the frozen control's `governance.frozen` error.
    pub fn check(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(VisionError::Cancelled);
        }
        self.scope.check_governance()?;
        Ok(())
    }
}
