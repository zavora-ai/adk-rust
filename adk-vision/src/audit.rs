//! `vision.*` audit records (spec 3.7 §9.3).
//!
//! Vision tool calls are already `tool_call` records on the governed path. These records add
//! kind `vision`, with the acting agent as actor and content by digest — never pixels
//! (V-I15) — in `payload_ref` and the extension fields:
//!
//! | Action | Written by | Fields |
//! |--------|------------|--------|
//! | [`VISION_CALL`] | `VisionToolset`, after each backend call | tool, backend, locality, image and egress digests, units, cost, region count, duration |
//! | [`VISION_EGRESS_RELEASED`], [`VISION_EGRESS_REFUSED`] | `EgressGate`, before the request is sent (V-I3) | source and egress digests, transform chain, redaction counts by class, destination, rule, decision, estimated cost, artifact |
//! | [`VISION_EDIT`] | `VisionToolset`, after an edit | input, operation and output digests |
//!
//! The records go to the trail a [`VisionCx`](crate::VisionCx) carries
//! ([`with_audit`](crate::VisionCx::with_audit)) — the run's trail, signed when its recorder
//! signs.

use adk_core::audit_log::{AuditDecision, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome};
use serde_json::{Value, json};

use crate::digest::Digest;
use crate::egress::{EgressDecision, EgressRecord};
use crate::runtime::CallRecord;

/// The audit record kind of every vision record.
pub const VISION_AUDIT_KIND: &str = "vision";
/// Action of the record one backend call writes.
pub const VISION_CALL: &str = "vision.call";
/// Action of the record an image released to a remote backend writes.
pub const VISION_EGRESS_RELEASED: &str = "vision.egress.released";
/// Action of the record an image the gate refused writes.
pub const VISION_EGRESS_REFUSED: &str = "vision.egress.refused";
/// Action of the record an edit writes.
pub const VISION_EDIT: &str = "vision.edit";

fn kind() -> AuditRecordKind {
    AuditRecordKind::Other(VISION_AUDIT_KIND.to_string())
}

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// The `vision.egress.released` or `vision.egress.refused` record of an egress entry.
pub(crate) fn egress_draft(record: &EgressRecord) -> AuditRecordDraft {
    match record {
        EgressRecord::Released(receipt) => {
            let (decision, rule) = match &receipt.decision {
                EgressDecision::Allowed { rule } => (AuditDecision::Allowed, *rule),
                EgressDecision::Approved { rule } => (AuditDecision::Approved, *rule),
            };
            let mut draft = AuditRecordDraft::new(kind(), VISION_EGRESS_RELEASED)
                .decision(decision)
                .outcome(AuditRecordOutcome::Succeeded)
                .payload_ref(receipt.egress.to_string())
                .ext("releaseId", json!(receipt.id))
                .ext("sourceDigest", json!(receipt.source))
                .ext("egressDigest", json!(receipt.egress))
                .ext("backend", json!(receipt.backend))
                .ext("destination", json!(receipt.hosts))
                .ext("dataClass", json!(receipt.data_class))
                .ext("chain", to_value(&receipt.chain))
                .ext("redactions", to_value(&receipt.redactions))
                .ext("rule", json!(rule))
                .ext("decisionKind", to_value(&receipt.decision));
            if let Some(cost) = receipt.estimated_cost_micro_usd {
                draft = draft.ext("estimatedCostMicroUsd", json!(cost));
            }
            if let Some(artifact) = &receipt.artifact {
                draft = draft.ext("artifact", to_value(artifact));
            }
            draft
        }
        EgressRecord::Refused(refusal) => {
            let mut draft = AuditRecordDraft::new(kind(), VISION_EGRESS_REFUSED)
                .decision(AuditDecision::Denied)
                .outcome(AuditRecordOutcome::NotRun)
                .payload_ref(refusal.source.to_string())
                .ext("sourceDigest", json!(refusal.source))
                .ext("backend", json!(refusal.backend))
                .ext("destination", json!(refusal.hosts))
                .ext("dataClass", json!(refusal.data_class))
                .ext("step", json!(refusal.step))
                .ext("reason", json!(refusal.reason));
            if let Some(egress) = refusal.egress {
                draft = draft.ext("egressDigest", json!(egress));
            }
            draft
        }
    }
}

/// The `vision.call` record of one backend call made for `tool`.
pub(crate) fn call_draft(tool: &str, record: &CallRecord) -> AuditRecordDraft {
    let mut draft = AuditRecordDraft::new(kind(), VISION_CALL)
        .outcome(AuditRecordOutcome::Succeeded)
        .ext("tool", json!(tool))
        .ext("task", to_value(&record.task))
        .ext("backend", json!(record.backend))
        .ext("locality", to_value(&record.locality))
        .ext("imageDigests", json!(record.images))
        .ext("units", to_value(&record.units))
        .ext("regions", json!(record.regions))
        .ext("durationMs", json!(record.duration_ms));
    if let Some(first) = record.images.first() {
        draft = draft.payload_ref(first.to_string());
    }
    if !record.egress.is_empty() {
        draft = draft.ext("egressDigests", json!(record.egress));
    }
    if let Some(cost) = record.cost_micro_usd {
        draft = draft.ext("costMicroUsd", json!(cost));
    }
    if let Some(reason) = &record.escalation {
        draft = draft.ext("escalation", to_value(reason));
    }
    draft
}

/// The `vision.edit` record of an edit.
pub(crate) fn edit_draft(input: Digest, operation: Digest, output: Digest) -> AuditRecordDraft {
    AuditRecordDraft::new(kind(), VISION_EDIT)
        .outcome(AuditRecordOutcome::Succeeded)
        .payload_ref(output.to_string())
        .ext("inputDigest", json!(input))
        .ext("operationDigest", json!(operation))
        .ext("outputDigest", json!(output))
}
