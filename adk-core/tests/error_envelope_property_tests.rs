//! Property-based tests for the structured error envelope.
//!
//! **Feature: structured-error-envelope**
//! - P1: Backward-compat constructors produce correct `is_{component}()` results
//! - P2: Retryable categories produce `is_retryable() == true` by default; explicit override works
//! - P3: Errors with source have `Error::source().is_some()`
//! - P4: `http_status_code()` maps correctly for all categories
//! - P5: `From` impls preserve source (tested in crate-specific tests)
//! - P6: `Display` contains component and message
//! - P7: `AdkError::new()` fields are accessible and correct
//! - P8: Backward-compat codes end with `.legacy`
//!
//! **Validates: Requirements 1.1–1.10, 3.5–3.8, 7.1–7.5, 10.1**

use adk_core::{AdkError, ErrorCategory, ErrorComponent, RetryHint};
use proptest::prelude::*;
use std::error::Error;

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

fn arb_component() -> impl Strategy<Value = ErrorComponent> {
    prop_oneof![
        Just(ErrorComponent::Agent),
        Just(ErrorComponent::Model),
        Just(ErrorComponent::Tool),
        Just(ErrorComponent::Session),
        Just(ErrorComponent::Artifact),
        Just(ErrorComponent::Memory),
        Just(ErrorComponent::Graph),
        Just(ErrorComponent::Realtime),
        Just(ErrorComponent::Code),
        Just(ErrorComponent::Server),
        Just(ErrorComponent::Auth),
        Just(ErrorComponent::Guardrail),
        Just(ErrorComponent::Eval),
        Just(ErrorComponent::Deploy),
    ]
}

fn arb_category() -> impl Strategy<Value = ErrorCategory> {
    prop_oneof![
        Just(ErrorCategory::InvalidInput),
        Just(ErrorCategory::Unauthorized),
        Just(ErrorCategory::Forbidden),
        Just(ErrorCategory::NotFound),
        Just(ErrorCategory::RateLimited),
        Just(ErrorCategory::Timeout),
        Just(ErrorCategory::Unavailable),
        Just(ErrorCategory::Cancelled),
        Just(ErrorCategory::Internal),
        Just(ErrorCategory::Unsupported),
    ]
}

fn arb_message() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 _.:/-]{1,100}"
}

// ---------------------------------------------------------------------------
// P1: Backward-compat constructors produce correct is_{component}() results
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: structured-error-envelope, Property 1: Backward-Compat Component Check**
    /// *For any* message, backward-compat constructors SHALL produce errors where
    /// `is_{component}()` returns true for the matching component.
    /// **Validates: Requirements 7.1, 7.5**
    #[test]
    fn prop_backward_compat_component_checks(msg in arb_message()) {
        let agent_err = AdkError::agent(&msg);
        prop_assert!(agent_err.is_agent(), "agent() should produce is_agent() == true");
        prop_assert!(!agent_err.is_model());

        let model_err = AdkError::model(&msg);
        prop_assert!(model_err.is_model(), "model() should produce is_model() == true");
        prop_assert!(!model_err.is_agent());

        let tool_err = AdkError::tool(&msg);
        prop_assert!(tool_err.is_tool(), "tool() should produce is_tool() == true");

        let session_err = AdkError::session(&msg);
        prop_assert!(session_err.is_session(), "session() should produce is_session() == true");

        let memory_err = AdkError::memory(&msg);
        prop_assert!(memory_err.is_memory(), "memory() should produce is_memory() == true");

        let artifact_err = AdkError::artifact(&msg);
        prop_assert!(artifact_err.is_artifact(), "artifact() should produce is_artifact() == true");

        let config_err = AdkError::config(&msg);
        prop_assert!(config_err.is_config(), "config() should produce is_config() == true");
    }
}

// ---------------------------------------------------------------------------
// P2: Retryable categories and override
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: structured-error-envelope, Property 2: Retryable Category Default**
    /// *For any* error with category in {RateLimited, Unavailable, Timeout},
    /// `is_retryable()` SHALL return true by default.
    /// **Validates: Requirements 3.5, 3.6, 3.7**
    #[test]
    fn prop_retryable_categories_default_true(
        component in arb_component(),
        msg in arb_message(),
    ) {
        for category in [ErrorCategory::RateLimited, ErrorCategory::Unavailable, ErrorCategory::Timeout] {
            let err = AdkError::new(component, category, "test.retryable", &msg);
            prop_assert!(
                err.is_retryable(),
                "category {:?} should be retryable by default", category
            );
        }
    }

    /// **Feature: structured-error-envelope, Property 2b: Retryable Override**
    /// *For any* error with explicit `should_retry: false`, `is_retryable()` SHALL
    /// return false regardless of category.
    /// **Validates: Requirement 3.8**
    #[test]
    fn prop_retryable_override_false(
        component in arb_component(),
        category in arb_category(),
        msg in arb_message(),
    ) {
        let err = AdkError::new(component, category, "test.override", &msg)
            .with_retry(RetryHint { should_retry: false, retry_after_ms: None, max_attempts: None });
        prop_assert!(
            !err.is_retryable(),
            "explicit should_retry=false should override category {:?}", category
        );
    }

    /// **Feature: structured-error-envelope, Property 2c: Non-Retryable Categories**
    /// *For any* error with category NOT in {RateLimited, Unavailable, Timeout},
    /// `is_retryable()` SHALL return false by default.
    /// **Validates: Requirement 3.5**
    #[test]
    fn prop_non_retryable_categories_default_false(
        component in arb_component(),
        msg in arb_message(),
    ) {
        for category in [
            ErrorCategory::InvalidInput,
            ErrorCategory::Unauthorized,
            ErrorCategory::Forbidden,
            ErrorCategory::NotFound,
            ErrorCategory::Cancelled,
            ErrorCategory::Internal,
            ErrorCategory::Unsupported,
        ] {
            let err = AdkError::new(component, category, "test.non_retryable", &msg);
            prop_assert!(
                !err.is_retryable(),
                "category {:?} should NOT be retryable by default", category
            );
        }
    }
}

// ---------------------------------------------------------------------------
// P3: Errors with source have source().is_some()
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: structured-error-envelope, Property 3: Source Preservation**
    /// *For any* error with a source, `Error::source()` SHALL return `Some`.
    /// **Validates: Requirements 1.7, 1.8**
    #[test]
    fn prop_source_preserved(
        component in arb_component(),
        category in arb_category(),
        msg in arb_message(),
        source_msg in arb_message(),
    ) {
        let source = std::io::Error::other(source_msg);
        let err = AdkError::new(component, category, "test.source", &msg)
            .with_source(source);
        prop_assert!(err.source().is_some(), "error with source should have source().is_some()");
    }

    /// *For any* error without a source, `Error::source()` SHALL return `None`.
    #[test]
    fn prop_no_source_returns_none(
        component in arb_component(),
        category in arb_category(),
        msg in arb_message(),
    ) {
        let err = AdkError::new(component, category, "test.no_source", &msg);
        prop_assert!(err.source().is_none(), "error without source should have source().is_none()");
    }
}

// ---------------------------------------------------------------------------
// P4: http_status_code() maps correctly for all categories
// ---------------------------------------------------------------------------

#[test]
fn prop_http_status_code_mapping() {
    let cases = vec![
        (ErrorCategory::InvalidInput, 400),
        (ErrorCategory::Unauthorized, 401),
        (ErrorCategory::Forbidden, 403),
        (ErrorCategory::NotFound, 404),
        (ErrorCategory::RateLimited, 429),
        (ErrorCategory::Timeout, 408),
        (ErrorCategory::Unavailable, 503),
        (ErrorCategory::Cancelled, 499),
        (ErrorCategory::Internal, 500),
        (ErrorCategory::Unsupported, 501),
    ];

    for (category, expected_status) in cases {
        let err = AdkError::new(ErrorComponent::Server, category, "test.status", "test");
        assert_eq!(
            err.http_status_code(),
            expected_status,
            "category {:?} should map to HTTP {}",
            category,
            expected_status
        );
    }
}

// ---------------------------------------------------------------------------
// P6: Display contains component and message
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: structured-error-envelope, Property 6: Display Format**
    /// *For any* `AdkError`, `Display` output SHALL contain the component and message.
    /// **Validates: Requirement 1.9**
    #[test]
    fn prop_display_contains_component_and_message(
        component in arb_component(),
        category in arb_category(),
        msg in arb_message(),
    ) {
        let err = AdkError::new(component, category, "test.display", &msg);
        let display = err.to_string();
        let component_str = format!("{component}");
        prop_assert!(
            display.contains(&component_str),
            "Display '{}' should contain component '{}'", display, component_str
        );
        prop_assert!(
            display.contains(&msg),
            "Display '{}' should contain message '{}'", display, msg
        );
    }
}

// ---------------------------------------------------------------------------
// P7: AdkError::new() fields are accessible and correct
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: structured-error-envelope, Property 7: Field Accessibility**
    /// *For any* `AdkError` constructed via `new()`, all fields SHALL be accessible
    /// and match the constructor arguments.
    /// **Validates: Requirements 1.1–1.7**
    #[test]
    fn prop_new_fields_accessible(
        component in arb_component(),
        category in arb_category(),
        msg in arb_message(),
    ) {
        let err = AdkError::new(component, category, "test.fields", &msg);
        prop_assert_eq!(err.component, component);
        prop_assert_eq!(err.category, category);
        prop_assert_eq!(err.code, "test.fields");
        prop_assert_eq!(&err.message, &msg);
    }

    /// Fields set via builder methods are accessible.
    #[test]
    fn prop_builder_fields_accessible(
        component in arb_component(),
        category in arb_category(),
        msg in arb_message(),
        provider in "[a-z]{3,10}",
        request_id in "[a-z0-9]{8,16}",
        status_code in 100u16..600u16,
    ) {
        let err = AdkError::new(component, category, "test.builder", &msg)
            .with_provider(&provider)
            .with_request_id(&request_id)
            .with_upstream_status(status_code);

        prop_assert_eq!(err.details.provider.as_deref(), Some(provider.as_str()));
        prop_assert_eq!(err.details.request_id.as_deref(), Some(request_id.as_str()));
        prop_assert_eq!(err.details.upstream_status_code, Some(status_code));
    }
}

// ---------------------------------------------------------------------------
// P8: Backward-compat codes end with .legacy
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: structured-error-envelope, Property 8: Legacy Code Suffix**
    /// *For any* backward-compat constructor, `code` SHALL end with `.legacy`.
    /// **Validates: Requirement 7.2**
    #[test]
    fn prop_backward_compat_codes_end_with_legacy(msg in arb_message()) {
        let constructors: Vec<(&str, AdkError)> = vec![
            ("agent", AdkError::agent(&msg)),
            ("model", AdkError::model(&msg)),
            ("tool", AdkError::tool(&msg)),
            ("session", AdkError::session(&msg)),
            ("memory", AdkError::memory(&msg)),
            ("artifact", AdkError::artifact(&msg)),
            ("config", AdkError::config(&msg)),
        ];

        for (name, err) in constructors {
            prop_assert!(
                err.code.ends_with(".legacy"),
                "backward-compat constructor '{}' should produce code ending with '.legacy', got '{}'",
                name, err.code
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Additional: to_problem_json() includes required fields
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// `to_problem_json()` SHALL include code, message, component, and category.
    /// **Validates: Requirement 10.2**
    #[test]
    fn prop_problem_json_includes_required_fields(
        component in arb_component(),
        category in arb_category(),
        msg in arb_message(),
    ) {
        let err = AdkError::new(component, category, "test.json", &msg);
        let json = err.to_problem_json();
        let error_obj = &json["error"];

        prop_assert!(error_obj.get("code").is_some(), "problem JSON should include 'code'");
        prop_assert!(error_obj.get("message").is_some(), "problem JSON should include 'message'");
        prop_assert!(error_obj.get("component").is_some(), "problem JSON should include 'component'");
        prop_assert!(error_obj.get("category").is_some(), "problem JSON should include 'category'");
        prop_assert_eq!(error_obj["code"].as_str(), Some("test.json"));
        prop_assert_eq!(error_obj["message"].as_str(), Some(msg.as_str()));
    }
}
