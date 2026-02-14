//! Tests for the events module.

use adk_realtime::{ClientEvent, ServerEvent, ToolCall, ToolResponse};

#[test]
fn test_tool_call_creation() {
    let call = ToolCall {
        call_id: "call_123".to_string(),
        name: "get_weather".to_string(),
        arguments: serde_json::json!({"location": "NYC"}),
    };

    assert_eq!(call.call_id, "call_123");
    assert_eq!(call.name, "get_weather");
}

#[test]
fn test_tool_response_creation() {
    let response = ToolResponse {
        call_id: "call_123".to_string(),
        output: serde_json::json!({"temperature": 72, "condition": "sunny"}),
    };

    assert_eq!(response.call_id, "call_123");
    assert!(response.output.get("temperature").is_some());
}

#[test]
fn test_tool_response_new() {
    let response = ToolResponse::new("call_456", serde_json::json!({"result": "ok"}));
    assert_eq!(response.call_id, "call_456");
}

#[test]
fn test_tool_response_from_string() {
    let response = ToolResponse::from_string("call_789", "Success!");
    assert_eq!(response.call_id, "call_789");
    assert_eq!(response.output, serde_json::json!("Success!"));
}

#[test]
fn test_client_event_audio_delta_serialization() {
    let event = ClientEvent::AudioDelta { event_id: None, audio: b"hello".to_vec() };

    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("input_audio_buffer.append"));
    // Audio should be base64-encoded on the wire
    assert!(json.contains("aGVsbG8=")); // base64("hello")
}

#[test]
fn test_client_event_audio_commit_serialization() {
    let event = ClientEvent::InputAudioBufferCommit;
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("input_audio_buffer.commit"));
}

#[test]
fn test_client_event_create_response_serialization() {
    let event = ClientEvent::ResponseCreate { config: None };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("response.create"));
}

#[test]
fn test_client_event_cancel_response_serialization() {
    let event = ClientEvent::ResponseCancel;
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("response.cancel"));
}

#[test]
fn test_server_event_audio_delta_deserialization() {
    // "base64audio==" decodes to bytes [0x6d, 0xab, 0x6d, 0xb6, 0xa9, 0xb6, 0xab, 0x6e]
    let json = r#"{
        "type": "response.audio.delta",
        "event_id": "evt_123",
        "response_id": "resp_456",
        "item_id": "item_789",
        "output_index": 0,
        "content_index": 0,
        "delta": "aGVsbG8="
    }"#;

    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::AudioDelta { event_id, delta, item_id, .. } => {
            assert_eq!(event_id, "evt_123");
            assert_eq!(delta, b"hello"); // decoded from base64
            assert_eq!(item_id, "item_789");
        }
        _ => panic!("Expected AudioDelta event"),
    }
}

#[test]
fn test_server_event_audio_delta_roundtrip() {
    let original = ServerEvent::AudioDelta {
        event_id: "evt_1".to_string(),
        response_id: "resp_1".to_string(),
        item_id: "item_1".to_string(),
        output_index: 0,
        content_index: 0,
        delta: vec![0x00, 0x01, 0x02, 0xFF],
    };

    let json = serde_json::to_string(&original).unwrap();
    let deserialized: ServerEvent = serde_json::from_str(&json).unwrap();

    match deserialized {
        ServerEvent::AudioDelta { delta, .. } => {
            assert_eq!(delta, vec![0x00, 0x01, 0x02, 0xFF]);
        }
        _ => panic!("Expected AudioDelta"),
    }
}

#[test]
fn test_server_event_text_delta_deserialization() {
    let json = r#"{
        "type": "response.text.delta",
        "event_id": "evt_123",
        "response_id": "resp_456",
        "item_id": "item_789",
        "output_index": 0,
        "content_index": 0,
        "delta": "Hello, world!"
    }"#;

    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::TextDelta { delta, .. } => {
            assert_eq!(delta, "Hello, world!");
        }
        _ => panic!("Expected TextDelta event"),
    }
}

#[test]
fn test_server_event_function_call_done_deserialization() {
    let json = r#"{
        "type": "response.function_call_arguments.done",
        "event_id": "evt_123",
        "response_id": "resp_456",
        "item_id": "item_789",
        "output_index": 0,
        "call_id": "call_abc",
        "name": "get_weather",
        "arguments": "{\"location\":\"NYC\"}"
    }"#;

    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::FunctionCallDone { call_id, name, arguments, .. } => {
            assert_eq!(call_id, "call_abc");
            assert_eq!(name, "get_weather");
            assert!(arguments.contains("NYC"));
        }
        _ => panic!("Expected FunctionCallDone event"),
    }
}

#[test]
fn test_server_event_speech_started_deserialization() {
    let json = r#"{
        "type": "input_audio_buffer.speech_started",
        "event_id": "evt_123",
        "audio_start_ms": 1500
    }"#;

    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::SpeechStarted { audio_start_ms, .. } => {
            assert_eq!(audio_start_ms, 1500);
        }
        _ => panic!("Expected SpeechStarted event"),
    }
}

#[test]
fn test_server_event_speech_stopped_deserialization() {
    let json = r#"{
        "type": "input_audio_buffer.speech_stopped",
        "event_id": "evt_456",
        "audio_end_ms": 3200
    }"#;

    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::SpeechStopped { audio_end_ms, .. } => {
            assert_eq!(audio_end_ms, 3200);
        }
        _ => panic!("Expected SpeechStopped event"),
    }
}

#[test]
fn test_server_event_error_deserialization() {
    let json = r#"{
        "type": "error",
        "event_id": "evt_123",
        "error": {
            "type": "rate_limit_error",
            "code": "rate_limit",
            "message": "Too many requests"
        }
    }"#;

    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::Error { error, .. } => {
            assert_eq!(error.error_type, "rate_limit_error");
            assert_eq!(error.code, Some("rate_limit".to_string()));
            assert_eq!(error.message, "Too many requests");
        }
        _ => panic!("Expected Error event"),
    }
}

#[test]
fn test_server_event_session_created_deserialization() {
    let json = r#"{
        "type": "session.created",
        "event_id": "evt_001",
        "session": {
            "id": "session_abc",
            "model": "gpt-4o-realtime"
        }
    }"#;

    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::SessionCreated { event_id, session } => {
            assert_eq!(event_id, "evt_001");
            assert!(session.get("id").is_some());
        }
        _ => panic!("Expected SessionCreated event"),
    }
}

#[test]
fn test_server_event_response_done_deserialization() {
    let json = r#"{
        "type": "response.done",
        "event_id": "evt_999",
        "response": {
            "id": "resp_123",
            "status": "completed"
        }
    }"#;

    let event: ServerEvent = serde_json::from_str(json).unwrap();
    match event {
        ServerEvent::ResponseDone { response, .. } => {
            assert_eq!(response.get("status").unwrap(), "completed");
        }
        _ => panic!("Expected ResponseDone event"),
    }
}

#[test]
fn test_server_event_unknown_type() {
    let json = r#"{
        "type": "some.unknown.event",
        "data": "whatever"
    }"#;

    let event: ServerEvent = serde_json::from_str(json).unwrap();
    assert!(matches!(event, ServerEvent::Unknown));
}
