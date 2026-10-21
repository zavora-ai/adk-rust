//! A self-hosted `adk-managed` runtime streams its own `SessionEvent` JSON; the client reads
//! it as it reads the service's (spec 3.1 §7.18: the managed runtime replays events to
//! `adk-enterprise` clients).

use adk_enterprise::{SessionEvent, StopReason};
use serde_json::json;

fn parse(value: serde_json::Value) -> SessionEvent {
    serde_json::from_value(value.clone()).unwrap_or_else(|error| panic!("{value}: {error}"))
}

#[test]
fn a_managed_runtime_idle_event_keeps_its_stop_reason() {
    for (reason, expected) in [
        (json!({"reason": "end_turn"}), StopReason::EndTurn),
        (json!({"reason": "max_tokens"}), StopReason::MaxTokens),
        (
            json!({"reason": "requires_action", "event_ids": ["call_1", "call_2"]}),
            StopReason::RequiresAction { event_ids: vec!["call_1".into(), "call_2".into()] },
        ),
    ] {
        let event = parse(json!({
            "type": "status.idle",
            "seq": 3,
            "stop_reason": reason,
            "usage": {"input_tokens": 10, "output_tokens": 4, "total_tokens": 14, "thinking_tokens": 2},
        }));
        let SessionEvent::StatusIdle { seq, stop_reason, usage } = event else {
            panic!("expected status.idle, got {event:?}");
        };
        assert_eq!((seq, stop_reason), (3, Some(expected)));
        assert_eq!(usage.map(|usage| usage.total_tokens), Some(14));
    }
}

#[test]
fn the_service_tag_still_parses_and_is_what_the_client_writes() {
    let reason: StopReason = serde_json::from_value(json!({"type": "end_turn"})).unwrap();
    assert_eq!(reason, StopReason::EndTurn);
    let written =
        serde_json::to_value(StopReason::RequiresAction { event_ids: vec!["a".into()] }).unwrap();
    assert_eq!(written, json!({"type": "requires_action", "event_ids": ["a"]}));
    assert!(serde_json::from_value::<StopReason>(json!({"reason": "requires_action"})).is_err());
    assert!(serde_json::from_value::<StopReason>(json!({"reason": "later"})).is_err());
}

#[test]
fn a_managed_runtime_error_event_parses_as_an_error() {
    let event =
        parse(json!({"type": "error", "code": "runner_error", "message": "boom", "seq": 7}));
    let SessionEvent::Error { seq, message, code } = event else {
        panic!("expected an error, got {event:?}");
    };
    assert_eq!((seq, message.as_str(), code.as_deref()), (7, "boom", Some("runner_error")));
}

#[test]
fn a_managed_runtime_mcp_tool_use_parses() {
    let event = parse(json!({
        "type": "agent.mcp_tool_use",
        "tool_use_id": "mcp_1",
        "name": "read",
        "input": {"path": "a"},
        "seq": 5,
    }));
    let SessionEvent::McpToolUse { seq, mcp_tool_use_id, server_name, name, .. } = event else {
        panic!("expected agent.mcp_tool_use, got {event:?}");
    };
    assert_eq!(
        (seq, mcp_tool_use_id.as_str(), server_name.as_str(), name.as_str()),
        (5, "mcp_1", "", "read")
    );
}
