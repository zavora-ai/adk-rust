//! K-I19 (spec 3.2 §9.2, §11): a ledger row never contains a payload string of the inputs
//! that produced it — not a reason, an argument, a key, a fingerprint, a prompt or a secret
//! — whichever source wrote it.

#![cfg(feature = "proc")]

use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind};
use adk_core::{
    ActingContext, RunState, SpendKey, SubjectKind, SubjectRef, ToolConfirmationDecision,
    ToolConfirmationRequest,
};
use adk_kernel::proc::{
    Activity, InMemoryLedgerStore, Ledger, LedgerKey, LedgerKind, activity_for_audit,
    activity_for_event,
};
use adk_kernel::{ExpiryAction, KernelEvent, PartitionId, SpendDelta, WakeOutcome};
use proptest::prelude::*;
use serde_json::json;

/// A payload no label, count or hash can contain by accident.
fn payload() -> impl Strategy<Value = String> {
    "[a-z]{3,8} PAYLOAD-[A-Z]{6}[0-9]{6}".prop_map(|text| text.to_string())
}

fn ledger() -> Ledger {
    Ledger::new(Arc::new(InMemoryLedgerStore::new()), LedgerKey::from_bytes([11; 32]))
}

/// Every kernel event with `p` in each of its free-text fields.
fn events(p: &str) -> Vec<KernelEvent> {
    let request = ToolConfirmationRequest {
        tool_name: "write_file".into(),
        function_call_id: Some(format!("call {p}")),
        args: json!({ "path": p, "content": format!("secret {p}") }),
        acting: None,
        origin: None,
    };
    let outcomes = [
        None,
        Some(WakeOutcome::Woke { pid: p.into() }),
        Some(WakeOutcome::Spawned { pid: p.into(), trigger: p.into() }),
        Some(WakeOutcome::Deduplicated { pid: p.into() }),
        Some(WakeOutcome::Expired { pid: p.into(), action: ExpiryAction::Run }),
    ];
    let mut events: Vec<KernelEvent> = outcomes
        .into_iter()
        .map(|outcome| KernelEvent::Wake {
            pid: Some(p.into()),
            source: format!("file:{p}"),
            key: format!("file:/home/{p}:Cargo.lock"),
            outcome,
        })
        .collect();
    events.extend([
        KernelEvent::Process {
            pid: p.into(),
            from: RunState::Running,
            to: RunState::Failed,
            reason: Some(format!("tool printed {p}")),
        },
        KernelEvent::Held { pid: p.into(), request },
        KernelEvent::Decided {
            pid: p.into(),
            fingerprint: p.into(),
            decision: ToolConfirmationDecision::Deny,
            by: SubjectRef::human(p),
        },
        KernelEvent::RuleHit { pid: p.into(), rule_id: p.into() },
        KernelEvent::Spend {
            partition: PartitionId::default(),
            key: SpendKey::org("adk-rust").with_agent("urn:adk:agent:adk-rust/harness"),
            delta: SpendDelta::new(42),
        },
        KernelEvent::Observation { id: p.into(), partition: PartitionId::default() },
        KernelEvent::Governance { frozen: true, reason: Some(format!("incident: {p}")) },
    ]);
    events
}

/// Audit records of every kind with `p` in their free-text fields.
fn records(p: &str) -> Vec<AuditRecordDraft> {
    let actor = AuditActor::new(
        &ActingContext::derived("adk-rust", "harness", "alice", SubjectKind::Human),
        "inv-1",
    );
    let with = |draft: AuditRecordDraft| {
        draft
            .actor(actor.clone())
            .args_digest(format!("sha256:{p}"))
            .payload_ref(p)
            .policy_ref(p)
            .ext("reason", json!(p))
            .ext("command", json!(format!("curl {p}")))
            .ext("sessionId", json!(p))
            .ext("nested", json!({ "text": p }))
    };
    vec![
        with(
            AuditRecordDraft::new(AuditRecordKind::ToolCall, "bash")
                .decision(AuditDecision::Denied),
        ),
        with(
            AuditRecordDraft::new(AuditRecordKind::ToolCall, "bash")
                .decision(AuditDecision::ApprovalRequired),
        ),
        with(
            AuditRecordDraft::new(AuditRecordKind::Approval, "transfer")
                .decision(AuditDecision::Approved),
        ),
        with(
            AuditRecordDraft::new(AuditRecordKind::Approval, "transfer")
                .decision(AuditDecision::Rejected),
        ),
        with(
            AuditRecordDraft::new(AuditRecordKind::Other("harness".into()), "harness.spend")
                .ext("modelCalls", json!(3))
                .ext("effort", json!(p)),
        ),
        with(
            AuditRecordDraft::new(
                AuditRecordKind::Other("harness".into()),
                "harness.loop.detected",
            )
            .ext("identicalCalls", json!(3))
            .ext("agent", json!(p)),
        ),
        with(AuditRecordDraft::new(
            AuditRecordKind::Other("harness".into()),
            "harness.mode.changed",
        )),
        with(AuditRecordDraft::new(AuditRecordKind::Payment, "pay")),
        with(AuditRecordDraft::new(AuditRecordKind::Configuration, "configure")),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// No row the kernel writes from its events carries a payload of the event.
    #[test]
    fn kernel_event_rows_carry_no_payload(p in payload()) {
        let ledger = ledger();
        for event in events(&p) {
            let Some(activity) = activity_for_event(&event) else { continue };
            let line = ledger.entry(activity).unwrap().to_jsonl().unwrap();
            prop_assert!(!line.contains("PAYLOAD"), "{} → {line}", event.name());
        }
    }

    /// No row written from an audit record carries a payload of the record.
    #[test]
    fn audit_rows_carry_no_payload(p in payload()) {
        let ledger = ledger();
        for draft in records(&p) {
            let Some(activity) = activity_for_audit(&draft) else { continue };
            let line = ledger.entry(activity).unwrap().to_jsonl().unwrap();
            prop_assert!(!line.contains("PAYLOAD"), "{} → {line}", draft.action);
        }
    }

    /// A subject reaches the row only as its HMAC, and a payload anywhere else is refused.
    #[test]
    fn subjects_are_hashed_and_content_is_refused(p in payload(), seconds in 0u64..100_000) {
        let ledger = ledger();
        let activity = Activity::new(LedgerKind::ToolCall, "urn:adk:agent:adk-rust/harness")
            .with_subject(format!("read_file\u{1f}{{\"path\":\"{p}\"}}"))
            .with_duration(std::time::Duration::from_secs(seconds))
            .with_count("attempt", 1);
        let line = ledger.entry(activity).unwrap().to_jsonl().unwrap();
        prop_assert!(!line.contains("PAYLOAD"));
        prop_assert!(ledger.entry(Activity::new(LedgerKind::ToolCall, p.clone())).is_err());
        prop_assert!(ledger.entry(Activity::new(LedgerKind::ToolCall, "a").with_effect(p.clone())).is_err());
        let mut meta = Activity::new(LedgerKind::ToolCall, "a");
        meta.meta.insert("path".into(), json!(p));
        prop_assert!(ledger.entry(meta).is_err());
    }
}
