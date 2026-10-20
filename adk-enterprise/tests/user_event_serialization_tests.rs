//! Serialization tests for `UserEvent` variants — validates their dotted `type` strings.

use adk_enterprise::{ConfirmationResult, ContentBlock, UserEvent};

#[test]
fn test_user_event_message_serialization() {
    let event = UserEvent::message("Hello, agent!");
    let json = serde_json::to_value(&event).unwrap();

    assert_eq!(json["type"], "user.message");
    assert_eq!(json["content"][0]["type"], "text");
    assert_eq!(json["content"][0]["text"], "Hello, agent!");
}

#[test]
fn test_user_event_message_deserialization() {
    let json =
        r#"{"type": "user.message", "content": [{"type": "text", "text": "Hello, agent!"}]}"#;
    let event: UserEvent = serde_json::from_str(json).unwrap();

    match event {
        UserEvent::Message { content } => {
            assert_eq!(content.len(), 1);
            match &content[0] {
                ContentBlock::Text { text } => assert_eq!(text, "Hello, agent!"),
                _ => panic!("expected Text block"),
            }
        }
        _ => panic!("expected Message variant"),
    }
}

#[test]
fn test_user_event_interrupt_serialization() {
    let event = UserEvent::interrupt();
    let json = serde_json::to_value(&event).unwrap();

    assert_eq!(json["type"], "user.interrupt");
    // Interrupt has no additional fields
    assert_eq!(json.as_object().unwrap().len(), 1);
}

#[test]
fn test_user_event_interrupt_deserialization() {
    let json = r#"{"type": "user.interrupt"}"#;
    let event: UserEvent = serde_json::from_str(json).unwrap();

    assert!(matches!(event, UserEvent::Interrupt));
}

#[test]
fn test_user_event_allow_tool_serialization() {
    let event = UserEvent::allow_tool("tool_123");
    let json = serde_json::to_value(&event).unwrap();

    assert_eq!(json["type"], "user.tool_confirmation");
    assert_eq!(json["tool_use_id"], "tool_123");
    assert_eq!(json["result"], "allow");
    assert!(json.get("deny_message").is_none());
}

#[test]
fn test_user_event_allow_tool_deserialization() {
    let json =
        r#"{"type": "user.tool_confirmation", "tool_use_id": "tool_123", "result": "allow"}"#;
    let event: UserEvent = serde_json::from_str(json).unwrap();

    match event {
        UserEvent::ToolConfirmation { tool_use_id, result, deny_message } => {
            assert_eq!(tool_use_id, "tool_123");
            assert_eq!(result, ConfirmationResult::Allow);
            assert_eq!(deny_message, None);
        }
        _ => panic!("expected ToolConfirmation variant"),
    }
}

#[test]
fn test_user_event_deny_tool_serialization() {
    let event = UserEvent::deny_tool("tool_456", "Not authorized");
    let json = serde_json::to_value(&event).unwrap();

    assert_eq!(json["type"], "user.tool_confirmation");
    assert_eq!(json["tool_use_id"], "tool_456");
    assert_eq!(json["result"], "deny");
    assert_eq!(json["deny_message"], "Not authorized");
}

#[test]
fn test_user_event_deny_tool_deserialization() {
    let json = r#"{"type": "user.tool_confirmation", "tool_use_id": "tool_456", "result": "deny", "deny_message": "Not authorized"}"#;
    let event: UserEvent = serde_json::from_str(json).unwrap();

    match event {
        UserEvent::ToolConfirmation { tool_use_id, result, deny_message } => {
            assert_eq!(tool_use_id, "tool_456");
            assert_eq!(result, ConfirmationResult::Deny);
            assert_eq!(deny_message, Some("Not authorized".to_string()));
        }
        _ => panic!("expected ToolConfirmation variant"),
    }
}

#[test]
fn test_user_event_custom_tool_result_serialization() {
    let event =
        UserEvent::custom_tool_result("tool_789", vec![ContentBlock::text("Result content here")]);
    let json = serde_json::to_value(&event).unwrap();

    assert_eq!(json["type"], "user.custom_tool_result");
    assert_eq!(json["custom_tool_use_id"], "tool_789");
    assert_eq!(json["content"][0]["type"], "text");
    assert_eq!(json["content"][0]["text"], "Result content here");
}

#[test]
fn test_user_event_custom_tool_result_deserialization() {
    let json = r#"{"type": "user.custom_tool_result", "custom_tool_use_id": "tool_789", "content": [{"type": "text", "text": "Result content here"}]}"#;
    let event: UserEvent = serde_json::from_str(json).unwrap();

    match event {
        UserEvent::CustomToolResult { custom_tool_use_id, content } => {
            assert_eq!(custom_tool_use_id, "tool_789");
            assert_eq!(content.len(), 1);
        }
        _ => panic!("expected CustomToolResult variant"),
    }
}

#[test]
fn test_user_event_define_outcome_serialization() {
    let event = UserEvent::define_outcome("Complete the report by end of day");
    let json = serde_json::to_value(&event).unwrap();

    assert_eq!(json["type"], "user.define_outcome");
    assert_eq!(json["criteria"], "Complete the report by end of day");
}

#[test]
fn test_user_event_define_outcome_deserialization() {
    let json =
        r#"{"type": "user.define_outcome", "criteria": "Complete the report by end of day"}"#;
    let event: UserEvent = serde_json::from_str(json).unwrap();

    match event {
        UserEvent::DefineOutcome { criteria } => {
            assert_eq!(criteria, "Complete the report by end of day");
        }
        _ => panic!("expected DefineOutcome variant"),
    }
}

#[test]
fn test_user_event_round_trip_all_variants() {
    let events = vec![
        UserEvent::message("test message"),
        UserEvent::interrupt(),
        UserEvent::allow_tool("id_1"),
        UserEvent::deny_tool("id_2", "reason"),
        UserEvent::custom_tool_result("id_3", vec![ContentBlock::text("result")]),
        UserEvent::define_outcome("criteria text"),
    ];

    for event in events {
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: UserEvent = serde_json::from_str(&json).unwrap();
        // Verify round-trip produces the same JSON
        let json2 = serde_json::to_string(&deserialized).unwrap();
        assert_eq!(json, json2);
    }
}
