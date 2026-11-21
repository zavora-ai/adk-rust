//! The stream's `vision.*` audit records (spec 3.7 §9.3): sources, the governor and
//! evidence, content by digest, never pixels (V-I15).

use adk_core::audit_log::{AuditRecordDraft, AuditRecordKind, AuditRecordOutcome};
use serde_json::{Value, json};

use super::governor::{GovernorChange, GovernorStep};
use crate::audit::VISION_AUDIT_KIND;
use crate::media::FrameSourceId;

/// A source started delivering frames.
pub const VISION_SOURCE_STARTED: &str = "vision.source.started";
/// A source paused: its quota is spent or the kill switch is frozen.
pub const VISION_SOURCE_PAUSED: &str = "vision.source.paused";
/// A paused source resumed.
pub const VISION_SOURCE_RESUMED: &str = "vision.source.resumed";
/// A source stopped: it ended, was cancelled or failed.
pub const VISION_SOURCE_STOPPED: &str = "vision.source.stopped";
/// The frame governor changed a stream's rate, escalation or pause state.
pub const VISION_GOVERNOR_STEP: &str = "vision.governor.step";
/// A frame was stored as evidence.
pub const VISION_EVIDENCE_KEPT: &str = "vision.evidence.kept";
/// A stored frame was deleted: its retention ran out, or its scope was erased.
pub const VISION_EVIDENCE_PURGED: &str = "vision.evidence.purged";

fn kind() -> AuditRecordKind {
    AuditRecordKind::Other(VISION_AUDIT_KIND.to_string())
}

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// A `vision.source.*` record with its extension fields.
pub(crate) fn source_draft(
    action: &str,
    source: &FrameSourceId,
    fields: Value,
) -> AuditRecordDraft {
    let mut draft = AuditRecordDraft::new(kind(), action)
        .outcome(AuditRecordOutcome::Succeeded)
        .ext("source", json!(source.0));
    if let Value::Object(fields) = fields {
        for (key, value) in fields {
            draft = draft.ext(key, value);
        }
    }
    draft
}

/// The `vision.governor.step` record of one governor change, and the `vision.source.paused`
/// or `.resumed` record that goes with a pause or a resume.
pub(crate) fn governor_drafts(
    source: &FrameSourceId,
    step: &GovernorStep,
) -> Vec<AuditRecordDraft> {
    let mut drafts = vec![
        AuditRecordDraft::new(kind(), VISION_GOVERNOR_STEP)
            .outcome(AuditRecordOutcome::Succeeded)
            .ext("source", json!(source.0))
            .ext("at", json!(step.at))
            .ext("change", to_value(&step.change))
            .ext("quota", to_value(&step.state)),
    ];
    match &step.change {
        GovernorChange::Paused { reason } => drafts.push(source_draft(
            VISION_SOURCE_PAUSED,
            source,
            json!({ "reason": reason, "quota": step.state }),
        )),
        GovernorChange::Resumed => drafts.push(source_draft(
            VISION_SOURCE_RESUMED,
            source,
            json!({ "reason": "periodStarted", "quota": step.state }),
        )),
        GovernorChange::RateLowered { .. }
        | GovernorChange::EscalationStopped { .. }
        | GovernorChange::RateRestored { .. }
        | GovernorChange::EscalationResumed => {}
    }
    drafts
}

/// A `vision.evidence.*` record, its payload the frame's digest.
pub(crate) fn evidence_draft(action: &str, digest: &str, fields: Value) -> AuditRecordDraft {
    let mut draft = AuditRecordDraft::new(kind(), action)
        .outcome(AuditRecordOutcome::Succeeded)
        .payload_ref(digest.to_string())
        .ext("digest", json!(digest));
    if let Value::Object(fields) = fields {
        for (key, value) in fields {
            draft = draft.ext(key, value);
        }
    }
    draft
}
