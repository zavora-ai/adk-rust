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

use adk_computer_use::runtime::binding::{
    validate_envelope_freshness, validate_lease, validate_receipt, validate_reservation,
};
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
        target: Some(target_for("app-1")),
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
        intent_id: "action-1".into(),
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
fn a_reservation_for_another_intent_is_rejected() {
    let mut reservation = reservation();
    reservation.intent_id = "action-2".into();

    let error = validate_reservation(&reservation, &envelope()).expect_err("must reject");
    assert!(error.to_string().contains("intent_id"), "{error}");
}

#[test]
fn an_inactive_or_expired_reservation_is_rejected() {
    let mut inactive = reservation();
    inactive.state = "released".into();
    let error = validate_reservation(&inactive, &envelope()).expect_err("must reject");
    assert!(error.to_string().contains("not active"), "{error}");

    let mut expired = reservation();
    expired.expires_at = "2020-01-01T00:00:00Z".into();
    let error = validate_reservation(&expired, &envelope()).expect_err("must reject");
    assert!(error.to_string().contains("expired"), "{error}");
}

#[test]
fn a_reservation_with_an_unreadable_expiry_is_rejected() {
    let mut reservation = reservation();
    reservation.expires_at = "later".into();

    let error = validate_reservation(&reservation, &envelope()).expect_err("must reject");
    assert!(error.to_string().contains("unreadable expiry"), "{error}");
}

#[test]
fn a_reservation_for_another_target_scope_is_rejected() {
    let mut wrong_app = reservation();
    wrong_app.scope.app_id = "app-2".into();
    let error = validate_reservation(&wrong_app, &envelope()).expect_err("must reject");
    assert!(error.to_string().contains("scope.app_id"), "{error}");

    let mut envelope = envelope();
    envelope.target.as_mut().unwrap().window_id = Some(serde_json::json!(42));
    let error = validate_reservation(&reservation(), &envelope).expect_err("must reject");
    assert!(error.to_string().contains("scope.window_id"), "{error}");
}

#[test]
fn a_reservation_without_target_evidence_is_rejected() {
    let mut envelope = envelope();
    envelope.target = None;

    let error = validate_reservation(&reservation(), &envelope).expect_err("must reject");
    assert!(error.to_string().contains("without target evidence"), "{error}");
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

#[test]
fn a_current_action_envelope_is_accepted() {
    assert!(validate_envelope_freshness(&envelope()).is_ok());
}

#[test]
fn an_expired_or_malformed_action_envelope_is_rejected() {
    let mut expired = envelope();
    expired.proposed_at = "2019-01-01T00:00:00Z".into();
    expired.expires_at = "2020-01-01T00:00:00Z".into();
    let error = validate_envelope_freshness(&expired).expect_err("must reject");
    assert!(error.to_string().contains("expired"), "{error}");

    let mut malformed = envelope();
    malformed.proposed_at = "eventually".into();
    let error = validate_envelope_freshness(&malformed).expect_err("must reject");
    assert!(error.to_string().contains("unreadable proposed_at"), "{error}");
}

#[test]
fn a_non_positive_action_validity_window_is_rejected() {
    let mut envelope = envelope();
    envelope.proposed_at = "2099-01-01T00:00:01Z".into();
    envelope.expires_at = "2099-01-01T00:00:00Z".into();

    let error = validate_envelope_freshness(&envelope).expect_err("must reject");
    assert!(error.to_string().contains("non-positive validity window"), "{error}");
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

// ── Lease limits: expiry, remaining budget, target boundaries ──────────
//
// The first version of `validate_lease` checked session, principal, agent, mode, `state ==
// "active"`, and `action_budget == 0`. The struct also carries `expires_at`, `actions_used`, and
// `boundaries`, and none were read — so an expired lease, a fully-consumed lease, and a lease
// scoped to a different application all passed. The PR that added the validator claimed it
// checked "remaining action budget"; it checked the total.

/// A target naming one application, for boundary checks.
fn target_for(app_id: &str) -> adk_computer_use::TargetEvidence {
    adk_computer_use::TargetEvidence {
        platform: "macos".into(),
        app_id: app_id.into(),
        pid: None,
        window_id: None,
        window_title_digest: None,
        display_id: None,
        role: None,
        label_digest: None,
        bounds: None,
        observation_id: "obs-1".into(),
        screenshot_hash: None,
        ui_tree_revision: None,
        confidence: 1.0,
        captured_at: "2026-01-01T00:00:00Z".into(),
    }
}

/// A lease bound to [`envelope`], expiring far in the future, with room in its budget.
fn usable_lease() -> ControlLease {
    let mut lease = lease();
    lease.expires_at = "2099-01-01T00:00:00Z".into();
    lease.action_budget = 5;
    lease.actions_used = 0;
    lease
}

#[test]
fn an_expired_lease_is_rejected() {
    let mut lease = usable_lease();
    lease.expires_at = "2020-01-01T00:00:00Z".into();

    let error =
        validate_lease(&lease, &envelope()).expect_err("an expired lease authorizes nothing");
    assert!(error.to_string().contains("expired"), "{error}");
}

#[test]
fn a_lease_with_an_unreadable_expiry_is_rejected() {
    // If validity cannot be established, the lease is not valid.
    let mut lease = usable_lease();
    lease.expires_at = "whenever".into();

    let error = validate_lease(&lease, &envelope()).expect_err("must reject");
    assert!(error.to_string().contains("unreadable expiry"), "{error}");
}

#[test]
fn a_fully_consumed_budget_is_rejected_even_though_the_total_is_nonzero() {
    // This is the case the original `action_budget == 0` check let through.
    let mut lease = usable_lease();
    lease.action_budget = 1;
    lease.actions_used = 1;

    let error = validate_lease(&lease, &envelope()).expect_err("must reject");
    let message = error.to_string();
    assert!(message.contains("no remaining action budget"), "{message}");
    assert!(message.contains("1 of 1 used"), "the counts must be reported: {message}");
}

#[test]
fn a_partially_consumed_budget_is_accepted() {
    let mut lease = usable_lease();
    lease.action_budget = 3;
    lease.actions_used = 2;

    assert!(validate_lease(&lease, &envelope()).is_ok(), "one action still remains");
}

#[test]
fn a_lease_scoped_to_another_app_is_rejected() {
    let mut envelope = envelope();
    envelope.target = Some(target_for("com.example.editor"));

    let mut lease = usable_lease();
    lease.boundaries.app_ids = vec!["com.example.browser".into()];

    let error = validate_lease(&lease, &envelope).expect_err("must reject");
    assert!(error.to_string().contains("boundaries.app_ids"), "{error}");
}

#[test]
fn a_lease_scoped_to_the_requested_app_is_accepted() {
    let mut envelope = envelope();
    envelope.target = Some(target_for("com.example.editor"));

    let mut lease = usable_lease();
    lease.boundaries.app_ids = vec!["com.example.editor".into()];

    assert!(validate_lease(&lease, &envelope).is_ok());
}

#[test]
fn an_unbounded_lease_still_authorizes_any_target() {
    // Empty boundaries mean "not scoped", which must not be read as "scoped to nothing".
    let mut envelope = envelope();
    envelope.target = Some(target_for("com.example.editor"));

    assert!(validate_lease(&usable_lease(), &envelope).is_ok());
}
