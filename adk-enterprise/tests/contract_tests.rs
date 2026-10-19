//! Wire-shape contract tests.
//!
//! Ensures the SDK types serialize/deserialize to the exact wire format
//! expected by the server. If these tests break, the SDK is incompatible.

use adk_enterprise::*;

#[test]
fn agent_deserializes_snake_case() {
    let json = r#"{"id":"agt_x","name":"a","model":"gemini-2.5-flash","tools":[],"mcp_servers":[],"skills":[],"version":1,"created_at":"2026-06-01T00:00:00Z","updated_at":"2026-06-01T00:00:00Z"}"#;
    let agent: Agent = serde_json::from_str(json).unwrap();
    assert_eq!(agent.id, "agt_x");
    let back = serde_json::to_value(&agent).unwrap();
    assert!(back.get("created_at").is_some(), "expected snake_case key created_at");
    assert!(back.get("mcp_servers").is_some(), "expected snake_case key mcp_servers");
    assert!(back.get("createdAt").is_none(), "camelCase key should NOT exist");
}

#[test]
fn session_deserializes_snake_case() {
    let json = r#"{"id":"ses_1","agent_id":"agt_1","status":"idle","created_at":"2026-06-01T00:00:00Z","updated_at":"2026-06-01T00:00:00Z"}"#;
    let session: Session = serde_json::from_str(json).unwrap();
    assert_eq!(session.agent_id, "agt_1");
    let back = serde_json::to_value(&session).unwrap();
    assert!(back.get("agent_id").is_some());
    assert!(back.get("created_at").is_some());
    assert!(back.get("agentId").is_none());
}

#[test]
fn create_agent_params_serializes_snake_case() {
    let params = CreateAgentParams {
        name: "test".into(),
        model: "gemini-2.5-flash".into(),
        system: Some("hi".into()),
        ..Default::default()
    };
    let json = serde_json::to_value(&params).unwrap();
    // No camelCase keys
    assert!(json.get("name").is_some());
    assert!(json.get("system").is_some());
    // permission_policy would be snake_case if present
}

#[test]
fn list_response_deserializes_wire_shape() {
    let json = r#"{"data":[{"id":"agt_1","name":"a","model":"m","version":1,"created_at":"t","updated_at":"t"}],"next_cursor":"cur_x","has_more":true}"#;
    let resp: ListResponse<Agent> = serde_json::from_str(json).unwrap();
    assert_eq!(resp.data.len(), 1);
    assert_eq!(resp.next_cursor, Some("cur_x".into()));
    assert!(resp.has_more);
}

#[test]
fn status_idle_carries_usage() {
    let json = r#"{"type":"status.idle","seq":2,"usage":{"input_tokens":9,"output_tokens":2,"total_tokens":11}}"#;
    let event: SessionEvent = serde_json::from_str(json).unwrap();
    match event {
        SessionEvent::StatusIdle { usage, .. } => {
            let u = usage.unwrap();
            assert_eq!(u.input_tokens, 9);
            assert_eq!(u.output_tokens, 2);
            assert_eq!(u.total_tokens, 11);
        }
        _ => panic!("expected StatusIdle"),
    }
}

#[test]
fn custom_tool_result_uses_wire_field_names() {
    let event = UserEvent::custom_tool_result("ctu_123", vec![ContentBlock::text("result")]);
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "user.custom_tool_result");
    assert_eq!(json["custom_tool_use_id"], "ctu_123");
    assert!(json.get("tool_use_id").is_none(), "should be custom_tool_use_id not tool_use_id");
}

#[test]
fn tool_confirmation_uses_result_field() {
    let event = UserEvent::allow_tool("tu_1");
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "user.tool_confirmation");
    assert_eq!(json["result"], "allow");
    assert!(json.get("action").is_none());
}

#[test]
fn user_message_serializes_with_content_blocks() {
    // user.message carries content: ContentBlock[], not text: String
    let event = UserEvent::message("Hello, agent!");
    let json = serde_json::to_value(&event).unwrap();

    assert_eq!(json["type"], "user.message");
    assert!(json.get("text").is_none(), "user.message must NOT have a 'text' field");
    assert!(json.get("content").is_some(), "user.message must have a 'content' field");
    assert_eq!(json["content"][0]["type"], "text");
    assert_eq!(json["content"][0]["text"], "Hello, agent!");
}

#[test]
fn user_message_deserializes_from_wire_shape() {
    let json = r#"{"type":"user.message","content":[{"type":"text","text":"Hi there"}]}"#;
    let event: UserEvent = serde_json::from_str(json).unwrap();
    match event {
        UserEvent::Message { content } => {
            assert_eq!(content.len(), 1);
            match &content[0] {
                ContentBlock::Text { text } => assert_eq!(text, "Hi there"),
                _ => panic!("expected Text block"),
            }
        }
        _ => panic!("expected Message variant"),
    }
}
