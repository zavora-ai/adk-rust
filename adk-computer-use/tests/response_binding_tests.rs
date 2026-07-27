//! Responses must be bound to the request, and committing is not verifying.
//!
//! **CU-01.** The adapter deserialized `ControlLease`, `TargetReservation`, and
//! `ExecutionReceipt` and returned them straight into graph state. Typed deserialization
//! proves shape, not provenance, and none of these structs has an invariant-enforcing
//! constructor — so a well-formed object belonging to another session, principal, action, or
//! mode parsed cleanly and was accepted. The external runtime stays authoritative; this is
//! the local check that stops a stale or confused response from propagating.
//!
//! **CU-02.** `verify()` returned `receipt.status == ReceiptStatus::Committed`. That collapses
//! two different claims — that the runtime performed the action, and that the intended effect
//! occurred — so a committed action whose effect did not happen was reported as completed,
//! from a node the reference graph labels "verify".

use adk_computer_use::runtime::binding::{validate_lease, validate_receipt, validate_reservation};
use adk_computer_use::{
    ActionClass, ActionEnvelope, ActionPostcondition, ControlLease, ExecutionMode,
    ExecutionReceipt, LeaseBoundaries, ReceiptStatus, TargetReservation, TargetReservationScope,
};

/// The request every response below is checked against.
fn envelope() -> ActionEnvelope {
    ActionEnvelope {
        action_id: "action-1".into(),
        session_id: "session-1".into(),
        execution_group_id: Some("group-1".into()),
        principal_id: "principal-1".into(),
        agent_id: Some("agent-1".into()),
        tool: "computer".into(),
        operation: "click".into(),
        action_class: ActionClass::EditReversible,
        requested_mode: ExecutionMode::Foreground,
        target: None,
        target_sensitivity: None,
        postcondition: None,
        reversible: true,
        external_side_effect: false,
        proposed_at: "2026-01-01T00:00:00Z".into(),
        expires_at: "2099-01-01T00:00:00Z".into(),
        args_digest: "a".repeat(64),
        resource: None,
        provenance: None,
        data_labels: Vec::new(),
    }
}

/// A lease correctly bound to [`envelope`].
fn lease() -> ControlLease {
    ControlLease {
        lease_id: "lease-1".into(),
        revision: 1,
        session_id: "session-1".into(),
        principal_id: "principal-1".into(),
        agent_id: Some("agent-1".into()),
        kind: "exclusive".into(),
        execution_mode: ExecutionMode::Foreground,
        state: "active".into(),
        acquired_at: None,
        expires_at: "2099-01-01T00:00:00Z".into(),
        action_budget: 1,
        actions_used: 0,
        boundaries: LeaseBoundaries {
            app_ids: Vec::new(),
            window_ids: Vec::new(),
            display_ids: Vec::new(),
        },
    }
}

/// A reservation correctly bound to [`envelope`].
fn reservation() -> TargetReservation {
    TargetReservation {
        reservation_id: "res-1".into(),
        revision: 1,
        intent_id: "intent-1".into(),
        session_id: "session-1".into(),
        principal_id: "principal-1".into(),
        execution_group_id: Some("group-1".into()),
        agent_id: Some("agent-1".into()),
        scope: TargetReservationScope { app_id: "app-1".into(), window_id: None },
        state: "active".into(),
        acquired_at: "2026-01-01T00:00:00Z".into(),
        expires_at: "2099-01-01T00:00:00Z".into(),
        terminal_reason: None,
    }
}

/// A receipt correctly bound to [`envelope`].
fn receipt() -> ExecutionReceipt {
    ExecutionReceipt {
        receipt_id: "receipt-1".into(),
        session_id: "session-1".into(),
        action_id: "action-1".into(),
        action_digest: "a".repeat(64),
        attempt: 1,
        status: ReceiptStatus::Committed,
        created_at: None,
        updated_at: None,
        result: None,
        error: None,
    }
}

// ── CU-01: one mismatched field at a time ─────────────────────────────

#[test]
fn a_correctly_bound_lease_is_accepted() {
    assert!(validate_lease(&lease(), &envelope()).is_ok());
}

#[test]
fn a_lease_for_another_session_is_rejected() {
    let mut lease = lease();
    lease.session_id = "session-2".into();
    let error = validate_lease(&lease, &envelope()).expect_err("must reject");
    assert!(error.to_string().contains("session_id"), "{error}");
}

#[test]
fn a_lease_for_another_principal_is_rejected() {
    let mut lease = lease();
    lease.principal_id = "principal-2".into();
    assert!(validate_lease(&lease, &envelope()).is_err());
}

#[test]
fn a_lease_for_another_agent_is_rejected() {
    let mut lease = lease();
    lease.agent_id = Some("agent-2".into());
    assert!(validate_lease(&lease, &envelope()).is_err());
}

#[test]
fn a_lease_for_another_execution_mode_is_rejected() {
    let mut lease = lease();
    lease.execution_mode = ExecutionMode::Background;
    let error = validate_lease(&lease, &envelope()).expect_err("must reject");
    assert!(error.to_string().contains("execution_mode"), "{error}");
}

#[test]
fn an_inactive_lease_is_rejected() {
    for state in ["expired", "released", "revoked"] {
        let mut lease = lease();
        lease.state = state.into();
        let error = validate_lease(&lease, &envelope())
            .expect_err("a lease that is not active authorizes nothing");
        assert!(error.to_string().contains("not active"), "{state}: {error}");
    }
}

#[test]
fn an_exhausted_lease_is_rejected() {
    let mut lease = lease();
    lease.action_budget = 0;
    let error = validate_lease(&lease, &envelope()).expect_err("must reject");
    assert!(error.to_string().contains("budget"), "{error}");
}

#[test]
fn a_correctly_bound_reservation_is_accepted() {
    assert!(validate_reservation(&reservation(), &envelope()).is_ok());
}

#[test]
fn a_reservation_for_another_session_or_principal_or_group_is_rejected() {
    let mut wrong_session = reservation();
    wrong_session.session_id = "session-2".into();
    assert!(validate_reservation(&wrong_session, &envelope()).is_err());

    let mut wrong_principal = reservation();
    wrong_principal.principal_id = "principal-2".into();
    assert!(validate_reservation(&wrong_principal, &envelope()).is_err());

    let mut wrong_group = reservation();
    wrong_group.execution_group_id = Some("group-2".into());
    assert!(validate_reservation(&wrong_group, &envelope()).is_err());
}

#[test]
fn a_correctly_bound_receipt_is_accepted() {
    let envelope = envelope();
    assert!(validate_receipt(&receipt(), &envelope, &envelope.args_digest).is_ok());
}

#[test]
fn a_receipt_for_another_action_is_rejected() {
    let envelope = envelope();
    let mut receipt = receipt();
    receipt.action_id = "action-2".into();
    let error =
        validate_receipt(&receipt, &envelope, &envelope.args_digest).expect_err("must reject");
    assert!(error.to_string().contains("action_id"), "{error}");
}

#[test]
fn a_receipt_with_a_different_digest_is_rejected_even_when_ids_match() {
    // The digest is what approval was granted against, so a mismatch means different work
    // regardless of the identifiers lining up.
    let envelope = envelope();
    let mut receipt = receipt();
    receipt.action_digest = "b".repeat(64);
    let error =
        validate_receipt(&receipt, &envelope, &envelope.args_digest).expect_err("must reject");
    assert!(error.to_string().contains("action_digest"), "{error}");
}

#[test]
fn a_receipt_for_another_session_is_rejected() {
    let envelope = envelope();
    let mut receipt = receipt();
    receipt.session_id = "session-2".into();
    assert!(validate_receipt(&receipt, &envelope, &envelope.args_digest).is_err());
}

// ── CU-02: committed is not verified ──────────────────────────────────

/// The postcondition a verifiable action would declare.
fn postcondition() -> ActionPostcondition {
    ActionPostcondition::Filesystem {
        path: "/tmp/report.txt".into(),
        exists: true,
        content_digest: Some("c".repeat(64)),
    }
}

#[test]
fn the_outcome_type_keeps_committed_and_verified_distinct() {
    use adk_computer_use::VerificationOutcome;

    let unverified = VerificationOutcome::CommittedUnverified { reason: "no evidence".to_string() };
    assert!(unverified.is_committed(), "the action was performed");
    assert!(
        !unverified.is_verified(),
        "but its effect was not observed, and a caller asking must not be told otherwise"
    );
    assert_eq!(unverified.status(), "committed_unverified");

    assert!(VerificationOutcome::Verified.is_verified());
    assert!(VerificationOutcome::Verified.is_committed());
    assert_eq!(VerificationOutcome::Verified.status(), "completed");

    let failed = VerificationOutcome::Failed { reason: "not committed".to_string() };
    assert!(!failed.is_committed());
    assert!(!failed.is_verified());
    assert_eq!(failed.status(), "verification_failed");
}

#[test]
fn a_declared_postcondition_carries_a_digest_to_bind_evidence_to() {
    // Verification compares an observed digest against this. Without it, evidence cannot be
    // bound to what was promised, which is why a digest-free postcondition can only be
    // satisfied by an explicit affirmative observation.
    match postcondition() {
        ActionPostcondition::Filesystem { content_digest, exists, .. } => {
            assert!(exists);
            assert_eq!(content_digest.as_deref().map(str::len), Some(64));
        }
        other => panic!("unexpected postcondition: {other:?}"),
    }
}
