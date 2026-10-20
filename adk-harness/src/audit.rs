//! `harness.*` audit records (spec 3.1 §9.3).

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail};
use adk_core::{Result, ToolContext};
use serde_json::Value;

/// Kind of every `harness.*` record (spec 3.1 §9.3: kind `Other` for harness actions).
pub(crate) const HARNESS_AUDIT_KIND: &str = "harness";

/// Writes `harness.*` records through a run's [`AuditTrail`], with the actor and
/// `ext.sessionId` the governed path uses.
#[derive(Clone, Debug)]
pub(crate) struct HarnessAudit {
    trail: AuditTrail,
    actor: AuditActor,
    session_id: String,
}

impl HarnessAudit {
    pub(crate) fn new(trail: AuditTrail, actor: AuditActor, session_id: impl Into<String>) -> Self {
        Self { trail, actor, session_id: session_id.into() }
    }

    /// The audit of the run a tool executes in, if the run records one.
    pub(crate) fn for_tool(ctx: &dyn ToolContext) -> Option<Self> {
        let trail = ctx.run_config()?.audit.clone()?;
        Some(Self::new(trail, AuditActor::for_context(ctx), ctx.session_id()))
    }

    pub(crate) fn trail(&self) -> &AuditTrail {
        &self.trail
    }

    /// Records `action` with `ext` (an object) as its extension fields.
    ///
    /// # Errors
    ///
    /// Under `AuditFailureMode::Block`, `audit.record_failed` when the record cannot be
    /// written; the caller refuses the action.
    pub(crate) async fn record(&self, action: &str, ext: Value) -> Result<()> {
        self.record_draft(
            AuditRecordDraft::new(AuditRecordKind::Other(HARNESS_AUDIT_KIND.to_string()), action),
            ext,
        )
        .await
    }

    /// Records `draft` with `ext` merged into its extension fields.
    pub(crate) async fn record_draft(&self, mut draft: AuditRecordDraft, ext: Value) -> Result<()> {
        if let Value::Object(fields) = ext {
            for (key, value) in fields {
                if !value.is_null() {
                    draft = draft.ext(key, value);
                }
            }
        }
        draft = draft
            .actor(self.actor.clone())
            .ext("sessionId", Value::String(self.session_id.clone()));
        self.trail.record(draft).await
    }
}
