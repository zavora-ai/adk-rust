use adk_computer_use::{ComputerUseEvaluator, SessionEvent};
use adk_eval::ToolUse;
use serde_json::json;

fn event(
    sequence: u64,
    event_type: &str,
    action_id: &str,
    payload: serde_json::Value,
) -> SessionEvent {
    SessionEvent {
        schema_version: 1,
        event_id: format!("event-{sequence}"),
        sequence,
        session_id: "session".into(),
        action_id: Some(action_id.into()),
        event_type: event_type.into(),
        at: "2026-07-13T10:00:00Z".into(),
        principal_id: Some("principal".into()),
        payload,
    }
}

#[test]
fn evaluator_accepts_one_leased_verified_commit() {
    let events = vec![
        event(
            1,
            "action.started",
            "a",
            json!({ "tool": "write_clipboard", "mode": "background", "leaseId": "lease" }),
        ),
        event(2, "action.verified", "a", json!({ "verified": true })),
        event(3, "action.committed", "a", json!({ "receiptId": "receipt" })),
    ];
    let expected = vec![ToolUse::new("write_clipboard").with_args(json!({
        "actionId": "a", "mode": "background",
    }))];
    let result = ComputerUseEvaluator::default().evaluate(&expected, &events);
    assert!(result.passed, "{:?}", result.violations);
    assert_eq!(result.mutations, 1);
}

#[test]
fn evaluator_rejects_duplicate_unleased_unverified_mutation() {
    let events = vec![
        event(
            1,
            "action.started",
            "a",
            json!({ "tool": "left_click", "mode": "foreground", "leaseId": null }),
        ),
        event(
            2,
            "action.started",
            "a",
            json!({ "tool": "left_click", "mode": "foreground", "leaseId": null }),
        ),
        event(3, "action.committed", "a", json!({ "receiptId": "receipt" })),
    ];
    let result = ComputerUseEvaluator::default().evaluate(&[], &events);
    assert!(!result.passed);
    assert!(result.violations.iter().any(|value| value.starts_with("duplicate_mutation")));
    assert!(result.violations.iter().any(|value| value.starts_with("commit_without_verification")));
}
