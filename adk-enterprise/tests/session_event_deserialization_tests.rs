//! Deserialization tests for `SessionEvent` variants — validates their dotted `type` strings.
//!
//! These tests ensure the SDK correctly deserializes all event types from the SSE stream,
//! including the forward-compatible `Unknown` catch-all for unrecognized event types.

use adk_enterprise::{ContentBlock, SessionEvent, StopReason};
use serde_json::json;

#[test]
fn test_session_event_message_deserialization() {
    let json = json!({
        "type": "agent.message",
        "seq": 1,
        "content": [
            {"type": "text", "text": "Hello, user!"}
        ]
    });

    let event: SessionEvent = serde_json::from_value(json).unwrap();
    match event {
        SessionEvent::Message { seq, content } => {
            assert_eq!(seq, 1);
            assert_eq!(content.len(), 1);
            match &content[0] {
                ContentBlock::Text { text } => assert_eq!(text, "Hello, user!"),
                _ => panic!("expected Text content block"),
            }
        }
        _ => panic!("expected Message variant"),
    }
}

#[test]
fn test_session_event_message_multiple_content_blocks() {
    let json = json!({
        "type": "agent.message",
        "seq": 5,
        "content": [
            {"type": "text", "text": "Here is an image:"},
            {"type": "image", "source": "https://example.com/img.png"},
            {"type": "file", "file_id": "file_abc123"}
        ]
    });

    let event: SessionEvent = serde_json::from_value(json).unwrap();
    match event {
        SessionEvent::Message { seq, content } => {
            assert_eq!(seq, 5);
            assert_eq!(content.len(), 3);
        }
        _ => panic!("expected Message variant"),
    }
}

#[test]
fn test_session_event_tool_use_deserialization() {
    let json = json!({
        "type": "agent.tool_use",
        "seq": 2,
        "tool_use_id": "tu_abc123",
        "name": "bash",
        "input": {"command": "ls -la"}
    });

    let event: SessionEvent = serde_json::from_value(json).unwrap();
    match event {
        SessionEvent::ToolUse { seq, tool_use_id, name, input } => {
            assert_eq!(seq, 2);
            assert_eq!(tool_use_id, "tu_abc123");
            assert_eq!(name, "bash");
            assert_eq!(input["command"], "ls -la");
        }
        _ => panic!("expected ToolUse variant"),
    }
}

#[test]
fn test_session_event_custom_tool_use_deserialization() {
    let json = json!({
        "type": "agent.custom_tool_use",
        "seq": 3,
        "custom_tool_use_id": "ctu_def456",
        "name": "get_weather",
        "input": {"city": "Tokyo"}
    });

    let event: SessionEvent = serde_json::from_value(json).unwrap();
    match event {
        SessionEvent::CustomToolUse { seq, custom_tool_use_id, name, input } => {
            assert_eq!(seq, 3);
            assert_eq!(custom_tool_use_id, "ctu_def456");
            assert_eq!(name, "get_weather");
            assert_eq!(input["city"], "Tokyo");
        }
        _ => panic!("expected CustomToolUse variant"),
    }
}

#[test]
fn test_session_event_mcp_tool_use_deserialization() {
    let json = json!({
        "type": "agent.mcp_tool_use",
        "seq": 4,
        "mcp_tool_use_id": "mcp_ghi789",
        "server_name": "slack-server",
        "name": "send_message",
        "input": {"channel": "#general", "text": "Hello!"}
    });

    let event: SessionEvent = serde_json::from_value(json).unwrap();
    match event {
        SessionEvent::McpToolUse { seq, mcp_tool_use_id, server_name, name, input } => {
            assert_eq!(seq, 4);
            assert_eq!(mcp_tool_use_id, "mcp_ghi789");
            assert_eq!(server_name, "slack-server");
            assert_eq!(name, "send_message");
            assert_eq!(input["channel"], "#general");
            assert_eq!(input["text"], "Hello!");
        }
        _ => panic!("expected McpToolUse variant"),
    }
}

#[test]
fn test_session_event_status_idle_with_end_turn() {
    let json = json!({
        "type": "status.idle",
        "seq": 10,
        "stop_reason": {"type": "end_turn"}
    });

    let event: SessionEvent = serde_json::from_value(json).unwrap();
    match event {
        SessionEvent::StatusIdle { seq, stop_reason, usage } => {
            assert_eq!(seq, 10);
            assert_eq!(stop_reason, Some(StopReason::EndTurn));
            assert!(usage.is_none());
        }
        _ => panic!("expected StatusIdle variant"),
    }
}

#[test]
fn test_session_event_status_idle_with_requires_action() {
    let json = json!({
        "type": "status.idle",
        "seq": 11,
        "stop_reason": {
            "type": "requires_action",
            "event_ids": ["evt_1", "evt_2"]
        }
    });

    let event: SessionEvent = serde_json::from_value(json).unwrap();
    match event {
        SessionEvent::StatusIdle { seq, stop_reason, .. } => {
            assert_eq!(seq, 11);
            match stop_reason {
                Some(StopReason::RequiresAction { event_ids }) => {
                    assert_eq!(event_ids, vec!["evt_1", "evt_2"]);
                }
                _ => panic!("expected RequiresAction stop reason"),
            }
        }
        _ => panic!("expected StatusIdle variant"),
    }
}

#[test]
fn test_session_event_status_idle_with_max_tokens() {
    let json = json!({
        "type": "status.idle",
        "seq": 12,
        "stop_reason": {"type": "max_tokens"}
    });

    let event: SessionEvent = serde_json::from_value(json).unwrap();
    match event {
        SessionEvent::StatusIdle { seq, stop_reason, .. } => {
            assert_eq!(seq, 12);
            assert_eq!(stop_reason, Some(StopReason::MaxTokens));
        }
        _ => panic!("expected StatusIdle variant"),
    }
}

#[test]
fn test_session_event_status_idle_no_stop_reason() {
    let json = json!({
        "type": "status.idle",
        "seq": 13,
        "stop_reason": null
    });

    let event: SessionEvent = serde_json::from_value(json).unwrap();
    match event {
        SessionEvent::StatusIdle { seq, stop_reason, usage } => {
            assert_eq!(seq, 13);
            assert_eq!(stop_reason, None);
            assert!(usage.is_none());
        }
        _ => panic!("expected StatusIdle variant"),
    }
}

#[test]
fn test_session_event_status_running_deserialization() {
    let json = json!({
        "type": "status.running",
        "seq": 6
    });

    let event: SessionEvent = serde_json::from_value(json).unwrap();
    match event {
        SessionEvent::StatusRunning { seq } => {
            assert_eq!(seq, 6);
        }
        _ => panic!("expected StatusRunning variant"),
    }
}

#[test]
fn test_session_event_error_deserialization() {
    let json = json!({
        "type": "agent.error",
        "seq": 7,
        "message": "Something went wrong",
        "code": "internal_error"
    });

    let event: SessionEvent = serde_json::from_value(json).unwrap();
    match event {
        SessionEvent::Error { seq, message, code } => {
            assert_eq!(seq, 7);
            assert_eq!(message, "Something went wrong");
            assert_eq!(code, Some("internal_error".to_string()));
        }
        _ => panic!("expected Error variant"),
    }
}

#[test]
fn test_session_event_error_without_code() {
    let json = json!({
        "type": "agent.error",
        "seq": 8,
        "message": "Rate limited",
        "code": null
    });

    let event: SessionEvent = serde_json::from_value(json).unwrap();
    match event {
        SessionEvent::Error { seq, message, code } => {
            assert_eq!(seq, 8);
            assert_eq!(message, "Rate limited");
            assert_eq!(code, None);
        }
        _ => panic!("expected Error variant"),
    }
}

#[test]
fn test_session_event_unknown_type_falls_through() {
    let json = json!({
        "type": "agent.new_feature",
        "seq": 99,
        "data": "something new"
    });

    let event: SessionEvent = serde_json::from_value(json).unwrap();
    assert!(matches!(event, SessionEvent::Unknown));
}

#[test]
fn test_session_event_unknown_completely_unrecognized() {
    let json = json!({
        "type": "some.future.event.type",
        "seq": 100,
        "payload": {"key": "value"}
    });

    let event: SessionEvent = serde_json::from_value(json).unwrap();
    assert!(matches!(event, SessionEvent::Unknown));
}

#[test]
fn test_session_event_round_trip_message() {
    let json_str = r#"{"type":"agent.message","seq":1,"content":[{"type":"text","text":"Hi"}]}"#;
    let event: SessionEvent = serde_json::from_str(json_str).unwrap();
    let reserialized = serde_json::to_string(&event).unwrap();
    let re_deserialized: SessionEvent = serde_json::from_str(&reserialized).unwrap();

    match (&event, &re_deserialized) {
        (
            SessionEvent::Message { seq: s1, content: c1 },
            SessionEvent::Message { seq: s2, content: c2 },
        ) => {
            assert_eq!(s1, s2);
            assert_eq!(c1.len(), c2.len());
        }
        _ => panic!("round-trip failed"),
    }
}

#[test]
fn test_session_event_round_trip_tool_use() {
    let original = json!({
        "type": "agent.tool_use",
        "seq": 42,
        "tool_use_id": "tu_xyz",
        "name": "web_search",
        "input": {"query": "rust async"}
    });

    let event: SessionEvent = serde_json::from_value(original.clone()).unwrap();
    let reserialized = serde_json::to_value(&event).unwrap();

    assert_eq!(reserialized["type"], "agent.tool_use");
    assert_eq!(reserialized["seq"], 42);
    assert_eq!(reserialized["tool_use_id"], "tu_xyz");
    assert_eq!(reserialized["name"], "web_search");
    assert_eq!(reserialized["input"]["query"], "rust async");
}

#[test]
fn test_session_event_round_trip_status_idle() {
    let original = json!({
        "type": "status.idle",
        "seq": 50,
        "stop_reason": {"type": "end_turn"}
    });

    let event: SessionEvent = serde_json::from_value(original).unwrap();
    let reserialized = serde_json::to_value(&event).unwrap();

    assert_eq!(reserialized["type"], "status.idle");
    assert_eq!(reserialized["seq"], 50);
    assert_eq!(reserialized["stop_reason"]["type"], "end_turn");
}

#[test]
fn test_stop_reason_serialization_end_turn() {
    let reason = StopReason::EndTurn;
    let json = serde_json::to_value(&reason).unwrap();
    assert_eq!(json["type"], "end_turn");
}

#[test]
fn test_stop_reason_serialization_requires_action() {
    let reason =
        StopReason::RequiresAction { event_ids: vec!["evt_a".to_string(), "evt_b".to_string()] };
    let json = serde_json::to_value(&reason).unwrap();
    assert_eq!(json["type"], "requires_action");
    assert_eq!(json["event_ids"], json!(["evt_a", "evt_b"]));
}

#[test]
fn test_stop_reason_serialization_max_tokens() {
    let reason = StopReason::MaxTokens;
    let json = serde_json::to_value(&reason).unwrap();
    assert_eq!(json["type"], "max_tokens");
}
