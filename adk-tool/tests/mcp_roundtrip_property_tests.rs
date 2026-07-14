//! Property-based tests for MCP JSON-RPC message serialization round-trip.
//!
//! These tests verify that for any valid MCP JSON-RPC message (request, response,
//! or notification), serializing to JSON and deserializing back produces a message
//! equal to the original, preserving method names, parameters, result values,
//! and error codes.

#![cfg(feature = "mcp")]
// Preserve round-trip coverage for MCP logging while rmcp keeps the deprecated
// SEP-2577 compatibility types available.
#![allow(deprecated)]

use proptest::prelude::*;
use rmcp::model::{
    CallToolResult, ContentBlock, ErrorCode, ErrorData, JsonRpcError, JsonRpcMessage, LoggingLevel,
    LoggingMessageNotificationParam, NumberOrString, ProgressNotificationParam, ProgressToken,
    RequestId,
};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

/// Generate an arbitrary request ID (number or string).
fn arb_request_id() -> impl Strategy<Value = RequestId> {
    prop_oneof![
        (0i64..=100_000i64).prop_map(NumberOrString::Number),
        "[a-z0-9-]{1,20}".prop_map(|s| NumberOrString::String(s.into())),
    ]
}

/// Generate an arbitrary error code.
fn arb_error_code() -> impl Strategy<Value = ErrorCode> {
    prop_oneof![
        Just(ErrorCode::PARSE_ERROR),
        Just(ErrorCode::INVALID_REQUEST),
        Just(ErrorCode::METHOD_NOT_FOUND),
        Just(ErrorCode::INVALID_PARAMS),
        Just(ErrorCode::INTERNAL_ERROR),
        Just(ErrorCode::RESOURCE_NOT_FOUND),
        (-32099i32..=-32000i32).prop_map(ErrorCode),
    ]
}

/// Generate an arbitrary error message.
fn arb_error_message() -> impl Strategy<Value = String> {
    "[A-Za-z0-9 .,!?:_-]{1,80}"
}

/// Generate an arbitrary error data value.
fn arb_error_data() -> impl Strategy<Value = Option<Value>> {
    prop_oneof![
        Just(None),
        "[a-z0-9 ]{1,30}".prop_map(|s| Some(Value::String(s))),
        (0i64..=1000i64).prop_map(|n| Some(Value::Number(n.into()))),
    ]
}

/// Generate an arbitrary JSON object for params/results.
fn arb_json_object() -> impl Strategy<Value = serde_json::Map<String, Value>> {
    prop::collection::hash_map("[a-z_]{1,10}", arb_json_value(), 0..=5)
        .prop_map(|map| map.into_iter().collect::<serde_json::Map<String, Value>>())
}

/// Generate a simple JSON value (no deep nesting to keep tests fast).
fn arb_json_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-1000i64..=1000i64).prop_map(|n| Value::Number(n.into())),
        "[a-zA-Z0-9 ]{0,30}".prop_map(Value::String),
    ]
}

/// Generate an arbitrary method name.
fn arb_method_name() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("tools/call".to_string()),
        Just("tools/list".to_string()),
        Just("resources/read".to_string()),
        Just("resources/list".to_string()),
        Just("prompts/get".to_string()),
        Just("prompts/list".to_string()),
        Just("ping".to_string()),
        Just("initialize".to_string()),
        Just("completion/complete".to_string()),
        "[a-z]+/[a-z_]+".prop_map(String::from),
    ]
}

/// Generate an arbitrary logging level.
fn arb_logging_level() -> impl Strategy<Value = LoggingLevel> {
    prop_oneof![
        Just(LoggingLevel::Debug),
        Just(LoggingLevel::Info),
        Just(LoggingLevel::Notice),
        Just(LoggingLevel::Warning),
        Just(LoggingLevel::Error),
        Just(LoggingLevel::Critical),
        Just(LoggingLevel::Alert),
        Just(LoggingLevel::Emergency),
    ]
}

// ---------------------------------------------------------------------------
// Property 6: MCP Message Serialization Round-Trip
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: one-point-zero-readiness, Property 6: MCP JSON-RPC Request Round-Trip**
    ///
    /// *For any* valid MCP JSON-RPC request message, serializing to JSON and
    /// deserializing back SHALL produce a message equal to the original,
    /// preserving method names and parameters.
    ///
    /// **Validates: Requirements 11.2**
    #[test]
    fn prop_jsonrpc_request_roundtrip(
        id in arb_request_id(),
        method in arb_method_name(),
        params in arb_json_object(),
    ) {
        // Build a raw JSON-RPC request
        let request_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        // Deserialize into the generic JsonRpcMessage type
        let message: JsonRpcMessage = serde_json::from_value(request_json.clone())
            .expect("Should deserialize valid JSON-RPC request");

        // Verify it's a request
        match &message {
            JsonRpcMessage::Request(req) => {
                prop_assert_eq!(&req.id, &id);
                prop_assert_eq!(&req.request.method, &method);
                prop_assert_eq!(&req.request.params, &params);
            }
            other => prop_assert!(false, "Expected Request, got: {:?}", other),
        }

        // Serialize back and verify equality
        let serialized = serde_json::to_value(&message)
            .expect("Should serialize back to JSON");
        prop_assert_eq!(&serialized, &request_json);
    }

    /// **Feature: one-point-zero-readiness, Property 6: MCP JSON-RPC Response Round-Trip**
    ///
    /// *For any* valid MCP JSON-RPC response message, serializing to JSON and
    /// deserializing back SHALL produce a message equal to the original,
    /// preserving result values.
    ///
    /// **Validates: Requirements 11.2**
    #[test]
    fn prop_jsonrpc_response_roundtrip(
        id in arb_request_id(),
        result in arb_json_object(),
    ) {
        // Build a raw JSON-RPC response
        let response_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        });

        // Deserialize into the generic JsonRpcMessage type
        let message: JsonRpcMessage = serde_json::from_value(response_json.clone())
            .expect("Should deserialize valid JSON-RPC response");

        // Verify it's a response
        match &message {
            JsonRpcMessage::Response(resp) => {
                prop_assert_eq!(&resp.id, &id);
                prop_assert_eq!(&resp.result, &result);
            }
            other => prop_assert!(false, "Expected Response, got: {:?}", other),
        }

        // Serialize back and verify equality
        let serialized = serde_json::to_value(&message)
            .expect("Should serialize back to JSON");
        prop_assert_eq!(&serialized, &response_json);
    }

    /// **Feature: one-point-zero-readiness, Property 6: MCP JSON-RPC Error Round-Trip**
    ///
    /// *For any* valid MCP JSON-RPC error message, serializing to JSON and
    /// deserializing back SHALL produce a message equal to the original,
    /// preserving error codes and messages.
    ///
    /// **Validates: Requirements 11.2**
    #[test]
    fn prop_jsonrpc_error_roundtrip(
        id in arb_request_id(),
        code in arb_error_code(),
        message in arb_error_message(),
        data in arb_error_data(),
    ) {
        let error = ErrorData {
            code,
            message: message.clone().into(),
            data: data.clone(),
        };

        let error_msg = JsonRpcError::new(Some(id.clone()), error.clone());

        // Serialize
        let serialized = serde_json::to_value(&error_msg)
            .expect("Should serialize JSON-RPC error");

        // Verify structure
        prop_assert_eq!(serialized["jsonrpc"].as_str(), Some("2.0"));
        prop_assert_eq!(&serialized["error"]["code"], &serde_json::json!(code.0));
        prop_assert_eq!(serialized["error"]["message"].as_str(), Some(message.as_str()));

        // Deserialize back
        let deserialized: JsonRpcError = serde_json::from_value(serialized)
            .expect("Should deserialize back to JsonRpcError");

        prop_assert_eq!(&deserialized.id, &Some(id));
        prop_assert_eq!(&deserialized.error.code, &code);
        prop_assert_eq!(deserialized.error.message.as_ref(), message.as_str());
        prop_assert_eq!(&deserialized.error.data, &data);
    }

    /// **Feature: one-point-zero-readiness, Property 6: MCP JSON-RPC Notification Round-Trip**
    ///
    /// *For any* valid MCP JSON-RPC notification message, serializing to JSON and
    /// deserializing back SHALL produce a message equal to the original,
    /// preserving method names and parameters.
    ///
    /// **Validates: Requirements 11.2**
    #[test]
    fn prop_jsonrpc_notification_roundtrip(
        method in arb_method_name(),
        params in arb_json_object(),
    ) {
        // Build a raw JSON-RPC notification (no id field)
        let notification_json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });

        // Deserialize into the generic JsonRpcMessage type
        let message: JsonRpcMessage = serde_json::from_value(notification_json.clone())
            .expect("Should deserialize valid JSON-RPC notification");

        // Verify it's a notification
        match &message {
            JsonRpcMessage::Notification(notif) => {
                prop_assert_eq!(&notif.notification.method, &method);
                prop_assert_eq!(&notif.notification.params, &params);
            }
            other => prop_assert!(false, "Expected Notification, got: {:?}", other),
        }

        // Serialize back and verify equality
        let serialized = serde_json::to_value(&message)
            .expect("Should serialize back to JSON");
        prop_assert_eq!(&serialized, &notification_json);
    }

    /// **Feature: one-point-zero-readiness, Property 6: MCP CallToolResult Round-Trip**
    ///
    /// *For any* valid CallToolResult with text content and error flag,
    /// serializing to JSON and deserializing back SHALL preserve all fields.
    ///
    /// **Validates: Requirements 11.2**
    #[test]
    fn prop_call_tool_result_roundtrip(
        text in "[a-zA-Z0-9 .,!?]{0,100}",
        is_error in any::<bool>(),
    ) {
        let content = vec![ContentBlock::text(&text)];
        let result = if is_error {
            CallToolResult::error(content.clone())
        } else {
            CallToolResult::success(content.clone())
        };

        // Serialize
        let serialized = serde_json::to_value(&result)
            .expect("Should serialize CallToolResult");

        // Deserialize back
        let deserialized: CallToolResult = serde_json::from_value(serialized)
            .expect("Should deserialize back to CallToolResult");

        // Verify fields preserved
        prop_assert_eq!(&deserialized.is_error, &Some(is_error));
        prop_assert_eq!(deserialized.content.len(), 1);

        // Verify text content preserved
        let original_text = result.content[0].as_text().map(|t| t.text.as_str());
        let roundtrip_text = deserialized.content[0].as_text().map(|t| t.text.as_str());
        prop_assert_eq!(roundtrip_text, original_text);
    }

    /// **Feature: one-point-zero-readiness, Property 6: MCP Logging Notification Round-Trip**
    ///
    /// *For any* valid logging notification with level, logger, and data,
    /// serializing to JSON and deserializing back SHALL preserve all fields.
    ///
    /// **Validates: Requirements 11.2**
    #[test]
    fn prop_logging_notification_roundtrip(
        level in arb_logging_level(),
        logger in prop::option::of("[a-z][a-z0-9.]{2,20}"),
        data_str in "[a-zA-Z0-9 ]{0,50}",
    ) {
        let mut param = LoggingMessageNotificationParam::new(
            level,
            Value::String(data_str.clone()),
        );
        if let Some(logger) = &logger {
            param = param.with_logger(logger);
        }

        // Serialize
        let serialized = serde_json::to_value(&param)
            .expect("Should serialize LoggingMessageNotificationParam");

        // Deserialize back
        let deserialized: LoggingMessageNotificationParam = serde_json::from_value(serialized)
            .expect("Should deserialize back");

        prop_assert_eq!(&deserialized.level, &level);
        prop_assert_eq!(&deserialized.logger, &logger);
        prop_assert_eq!(&deserialized.data, &Value::String(data_str));
    }

    /// **Feature: one-point-zero-readiness, Property 6: MCP Progress Notification Round-Trip**
    ///
    /// *For any* valid progress notification with token, progress value, and total,
    /// serializing to JSON and deserializing back SHALL preserve all fields.
    ///
    /// **Validates: Requirements 11.2**
    #[test]
    fn prop_progress_notification_roundtrip(
        token_id in 0i64..=100_000i64,
        progress in 0.0f64..=100.0f64,
        total in prop::option::of(1.0f64..=100.0f64),
        message in prop::option::of("[a-zA-Z0-9 ]{1,30}"),
    ) {
        let mut param = ProgressNotificationParam::new(
            ProgressToken(NumberOrString::Number(token_id)),
            progress,
        );
        if let Some(total) = total {
            param = param.with_total(total);
        }
        if let Some(message) = &message {
            param = param.with_message(message);
        }

        // Serialize
        let serialized = serde_json::to_value(&param)
            .expect("Should serialize ProgressNotificationParam");

        // Deserialize back
        let deserialized: ProgressNotificationParam = serde_json::from_value(serialized)
            .expect("Should deserialize back");

        prop_assert_eq!(
            &deserialized.progress_token,
            &ProgressToken(NumberOrString::Number(token_id))
        );
        // Float comparison with tolerance
        let progress_diff = (deserialized.progress - progress).abs();
        prop_assert!(
            progress_diff < 1e-10,
            "Progress mismatch: {} vs {}",
            deserialized.progress,
            progress
        );
        if let (Some(expected_total), Some(actual_total)) = (total, deserialized.total) {
            let total_diff = (actual_total - expected_total).abs();
            prop_assert!(
                total_diff < 1e-10,
                "Total mismatch: {} vs {}",
                actual_total,
                expected_total
            );
        } else {
            prop_assert_eq!(&deserialized.total.is_some(), &total.is_some());
        }
        prop_assert_eq!(&deserialized.message, &message);
    }

    /// **Feature: one-point-zero-readiness, Property 6: MCP ErrorData Round-Trip**
    ///
    /// *For any* valid ErrorData with code, message, and optional data,
    /// serializing to JSON and deserializing back SHALL preserve all fields.
    ///
    /// **Validates: Requirements 11.2**
    #[test]
    fn prop_error_data_roundtrip(
        code in arb_error_code(),
        message in arb_error_message(),
        data in arb_error_data(),
    ) {
        let error = ErrorData {
            code,
            message: message.clone().into(),
            data: data.clone(),
        };

        // Serialize
        let serialized = serde_json::to_value(&error)
            .expect("Should serialize ErrorData");

        // Deserialize back
        let deserialized: ErrorData = serde_json::from_value(serialized)
            .expect("Should deserialize back to ErrorData");

        prop_assert_eq!(&deserialized.code, &code);
        prop_assert_eq!(deserialized.message.as_ref(), message.as_str());
        prop_assert_eq!(&deserialized.data, &data);
    }
}
