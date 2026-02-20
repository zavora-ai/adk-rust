//! Property tests for Anthropic structured error types.

use adk_core::AdkError;
use adk_model::anthropic::AnthropicApiError;
use proptest::prelude::*;

/// Generate arbitrary non-empty strings for error fields.
fn arb_error_type() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("invalid_request_error".to_string()),
        Just("authentication_error".to_string()),
        Just("permission_error".to_string()),
        Just("not_found_error".to_string()),
        Just("rate_limit_error".to_string()),
        Just("api_error".to_string()),
        Just("overloaded_error".to_string()),
        "[a-z_]{3,30}".prop_map(String::from),
    ]
}

fn arb_message() -> impl Strategy<Value = String> {
    "[A-Za-z0-9 .,!?:;'-]{1,200}".prop_map(String::from)
}

fn arb_status_code() -> impl Strategy<Value = u16> {
    prop_oneof![
        Just(400u16),
        Just(401),
        Just(403),
        Just(404),
        Just(429),
        Just(500),
        Just(529),
        (400u16..600),
    ]
}

fn arb_request_id() -> impl Strategy<Value = Option<String>> {
    prop_oneof![Just(None), "req_[a-zA-Z0-9]{8,24}".prop_map(|s| Some(s)),]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: anthropic-deep-integration, Property 8: Error context preservation**
    /// *For any* Anthropic API error response with a type string, message string,
    /// HTTP status code, and optional request-id header, the resulting
    /// `AnthropicApiError` SHALL preserve all four fields, and its `Display`
    /// output SHALL contain the type, message, and status code (and request-id
    /// when present).
    /// **Validates: Requirements 4.1, 4.2, 4.4**
    #[test]
    fn prop_error_context_preservation(
        error_type in arb_error_type(),
        message in arb_message(),
        status_code in arb_status_code(),
        request_id in arb_request_id(),
    ) {
        let err = AnthropicApiError {
            error_type: error_type.clone(),
            message: message.clone(),
            status_code,
            request_id: request_id.clone(),
        };

        // Fields are preserved
        prop_assert_eq!(&err.error_type, &error_type);
        prop_assert_eq!(&err.message, &message);
        prop_assert_eq!(err.status_code, status_code);
        prop_assert_eq!(&err.request_id, &request_id);

        // Display contains all required fields
        let display = err.to_string();
        prop_assert!(
            display.contains(&error_type),
            "Display missing error_type: {display}"
        );
        prop_assert!(
            display.contains(&message),
            "Display missing message: {display}"
        );
        prop_assert!(
            display.contains(&status_code.to_string()),
            "Display missing status_code: {display}"
        );

        // Request ID present in display when Some
        if let Some(ref rid) = request_id {
            prop_assert!(
                display.contains(rid),
                "Display missing request_id: {display}"
            );
        }

        // From<AnthropicApiError> for AdkError preserves context
        let adk_err: AdkError = err.into();
        let adk_display = adk_err.to_string();
        prop_assert!(
            adk_display.contains(&error_type),
            "AdkError missing error_type: {adk_display}"
        );
        prop_assert!(
            adk_display.contains(&status_code.to_string()),
            "AdkError missing status_code: {adk_display}"
        );
    }
}
