use adk_computer_use::{
    ActionPostcondition, ActionPreview, AdkEvaluationReceipt, ApprovalGrant, ApprovalGrantScope,
    ControlLease, ExecutionReceipt, RuntimeSession, SafetyCorpus, SessionDeletionResult,
    SessionEvent, SessionFollowUp, TargetReservation, TargetSensitivityAssessment,
    TargetSensitivityEvidence, TargetSensitivitySignal, TargetSensitivitySource,
};

#[test]
fn types_round_trip_canonical_fixtures() {
    let preview: ActionPreview =
        serde_json::from_str(include_str!("../fixtures/v8/action-preview.json")).unwrap();
    let receipt: ExecutionReceipt =
        serde_json::from_str(include_str!("../fixtures/v8/execution-receipt.json")).unwrap();
    let event: SessionEvent =
        serde_json::from_str(include_str!("../fixtures/v8/session-event.json")).unwrap();
    let follow_up: SessionFollowUp =
        serde_json::from_str(include_str!("../fixtures/v8/session-follow-up.json")).unwrap();
    let lease: ControlLease =
        serde_json::from_str(include_str!("../fixtures/v8/control-lease.json")).unwrap();
    let reservation: TargetReservation =
        serde_json::from_str(include_str!("../fixtures/v8/target-reservation.json")).unwrap();
    let completed: RuntimeSession =
        serde_json::from_str(include_str!("../fixtures/v8/session-completion.json")).unwrap();
    let deletion: SessionDeletionResult =
        serde_json::from_str(include_str!("../fixtures/v8/session-deletion.json")).unwrap();
    let safety: SafetyCorpus =
        serde_json::from_str(include_str!("../fixtures/v8/safety-corpus.json")).unwrap();
    let evaluation: AdkEvaluationReceipt =
        serde_json::from_str(include_str!("../fixtures/v8/adk-evaluation-receipt-7.0.0.json"))
            .unwrap();
    let approval: ApprovalGrant =
        serde_json::from_str(include_str!("../fixtures/v8/approval-grant.json")).unwrap();

    assert_eq!(preview.envelope.action_id, receipt.action_id);
    assert!(matches!(
        preview.envelope.postcondition,
        Some(ActionPostcondition::UiElement { exists: true, .. })
    ));
    let sensitivity = preview.envelope.target_sensitivity.as_ref().unwrap();
    assert_eq!(sensitivity.assessment(), TargetSensitivityAssessment::NonSensitive);
    assert_eq!(sensitivity.source(), TargetSensitivitySource::Accessibility);
    assert_eq!(sensitivity.fields_checked(), 1);
    assert_eq!(preview.envelope.session_id, event.session_id);
    assert_eq!(follow_up.session_id, event.session_id);
    assert_eq!(follow_up.principal_id, event.principal_id.clone().unwrap());
    assert_eq!(lease.session_id, event.session_id);
    assert_eq!(reservation.execution_group_id.as_deref(), Some("group-0001"));
    assert_eq!(reservation.scope.window_id, Some(serde_json::json!(42)));
    assert_eq!(completed.session_id, event.session_id);
    assert_eq!(deletion.session_id, event.session_id);
    assert_eq!(deletion.deleted_events, 14);
    assert_eq!(deletion.deleted_evidence_frames, 4);
    assert!(deletion.retention_marker_id.is_some());
    assert!(completed.completion.unwrap().postconditions[0].satisfied);
    assert_eq!(safety.schema_version, 1);
    assert_eq!(safety.scenarios.len(), 4);
    assert!(evaluation.verify());
    assert_eq!(approval.scope(), ApprovalGrantScope::SessionOperation);
    assert_eq!(approval.grant_id(), "grant-0001");
    assert_eq!(approval.remaining_uses(), 10);
    assert_eq!(evaluation.claims.crash_points_covered, 2);
    assert_eq!(evaluation.claims.duplicate_mutations, 0);
    assert!(safety.scenarios.iter().any(|scenario| {
        scenario.id == "crash-after-effect-no-replay"
            && scenario.expected.error.as_deref() == Some("indeterminate")
            && scenario.expected.replay_effects == Some(1)
    }));
    assert_eq!(
        serde_json::from_value::<ActionPreview>(serde_json::to_value(preview).unwrap())
            .unwrap()
            .envelope
            .action_id,
        "action-0001"
    );
}

#[test]
fn approval_scope_contract_rejects_raw_or_broadened_authority() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/v8/approval-grant.json")).unwrap();
    let valid: ApprovalGrant = serde_json::from_value(fixture.clone()).unwrap();
    assert!(valid.scope_digest().bytes().all(|byte| byte == b'b'));

    let mut raw = fixture.clone();
    raw["value"] = serde_json::json!("must-not-cross");
    assert!(serde_json::from_value::<ApprovalGrant>(raw).is_err());

    let mut dangerous = fixture.clone();
    dangerous["tool"] = serde_json::json!("notification");
    dangerous["actionClass"] = serde_json::json!("communicate_external");
    assert!(serde_json::from_value::<ApprovalGrant>(dangerous).is_err());

    let mut exact = fixture;
    exact["scope"] = serde_json::json!("exact_action");
    exact["remainingUses"] = serde_json::json!(1);
    assert!(serde_json::from_value::<ApprovalGrant>(exact).is_err());
}

#[test]
fn process_postcondition_rejects_running_true_on_both_wire_directions() {
    assert!(
        serde_json::from_value::<ActionPostcondition>(serde_json::json!({
            "kind": "process",
            "pid": 42,
            "running": true
        }))
        .is_err()
    );
    assert!(serde_json::to_value(ActionPostcondition::Process { pid: 42, running: true }).is_err());
}

#[test]
fn sensitivity_contract_rejects_unrecognized_or_raw_value_fields() {
    let valid: TargetSensitivityEvidence = serde_json::from_value(serde_json::json!({
        "assessment": "sensitive",
        "source": "accessibility",
        "signals": ["uia_is_password"],
        "fieldsChecked": 1,
        "observedAt": "2026-07-13T12:00:00.000Z"
    }))
    .unwrap();
    assert_eq!(valid.signals(), &[TargetSensitivitySignal::UiaIsPassword]);
    assert!(
        serde_json::from_value::<TargetSensitivityEvidence>(serde_json::json!({
            "assessment": "sensitive",
            "source": "accessibility",
            "signals": ["model_supplied_secret"],
            "fieldsChecked": 1,
            "observedAt": "2026-07-13T12:00:00.000Z"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<TargetSensitivityEvidence>(serde_json::json!({
            "assessment": "sensitive",
            "source": "accessibility",
            "signals": ["uia_is_password"],
            "fieldsChecked": 1,
            "observedAt": "2026-07-13T12:00:00.000Z",
            "value": "must-not-cross"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<TargetSensitivityEvidence>(serde_json::json!({
            "assessment": "non_sensitive",
            "source": "unavailable",
            "signals": [],
            "fieldsChecked": 0,
            "observedAt": "2026-07-13T12:00:00.000Z"
        }))
        .is_err()
    );
}
