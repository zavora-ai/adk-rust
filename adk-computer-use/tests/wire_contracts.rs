use adk_computer_use::{
    ActionPreview, ControlLease, ExecutionReceipt, RuntimeSession, SafetyCorpus,
    SessionDeletionResult, SessionEvent, TargetReservation,
};

#[test]
fn types_round_trip_canonical_v8_fixtures() {
    let preview: ActionPreview =
        serde_json::from_str(include_str!("../fixtures/v8/action-preview.json")).unwrap();
    let receipt: ExecutionReceipt =
        serde_json::from_str(include_str!("../fixtures/v8/execution-receipt.json")).unwrap();
    let event: SessionEvent =
        serde_json::from_str(include_str!("../fixtures/v8/session-event.json")).unwrap();
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

    assert_eq!(preview.envelope.action_id, receipt.action_id);
    assert_eq!(preview.envelope.session_id, event.session_id);
    assert_eq!(lease.session_id, event.session_id);
    assert_eq!(reservation.execution_group_id.as_deref(), Some("group-0001"));
    assert_eq!(reservation.scope.window_id, Some(serde_json::json!(42)));
    assert_eq!(completed.session_id, event.session_id);
    assert_eq!(deletion.session_id, event.session_id);
    assert_eq!(deletion.deleted_events, 14);
    assert!(deletion.retention_marker_id.is_some());
    assert!(completed.completion.unwrap().postconditions[0].satisfied);
    assert_eq!(safety.schema_version, 1);
    assert_eq!(safety.scenarios.len(), 4);
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
