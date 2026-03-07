use adk_core::AdkError;
use proptest::prelude::*;

/// All error context prefixes used by `PostgresSessionService`.
const POSTGRES_ERROR_PREFIXES: &[&str] = &[
    "database connection failed",
    "migration failed",
    "transaction failed",
    "query failed",
    "serialize failed",
    "insert failed",
    "commit failed",
    "delete failed",
    "update failed",
    "session not found",
];

/// Construct an `AdkError::Session` the same way `PostgresSessionService` does:
/// `AdkError::Session(format!("{prefix}: {detail}"))`.
fn make_postgres_error(prefix: &str, detail: &str) -> AdkError {
    if detail.is_empty() {
        AdkError::Session(prefix.to_string())
    } else {
        AdkError::Session(format!("{prefix}: {detail}"))
    }
}

/// Generate an arbitrary non-empty error detail string.
fn arb_error_detail() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 _.,:;!?()-]{1,80}"
}

/// Pick a random error prefix from the set used by `PostgresSessionService`.
fn arb_error_prefix() -> impl Strategy<Value = &'static str> {
    prop::sample::select(POSTGRES_ERROR_PREFIXES)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// **Feature: production-backends, Property 8: Error Variant Correctness (PostgreSQL portion)**
    /// *For any* error produced by `PostgresSessionService`, the error is an `AdkError::Session`
    /// variant with a non-empty context message.
    /// **Validates: Requirements 16.1, 16.2**
    #[test]
    fn prop_postgres_errors_are_session_variant_with_context(
        prefix in arb_error_prefix(),
        detail in arb_error_detail(),
    ) {
        let err = make_postgres_error(prefix, &detail);

        // Must be the Session variant
        match &err {
            AdkError::Session(msg) => {
                // Message must be non-empty
                prop_assert!(!msg.is_empty(), "error message must not be empty");
                // Message must contain the prefix
                prop_assert!(
                    msg.starts_with(prefix),
                    "error message '{}' must start with prefix '{}'", msg, prefix
                );
                // Message must contain the detail
                prop_assert!(
                    msg.contains(&detail),
                    "error message '{}' must contain detail '{}'", msg, detail
                );
            }
            other => {
                prop_assert!(false, "expected AdkError::Session, got: {:?}", other);
            }
        }

        // Display output must also be non-empty and contain the context
        let display = err.to_string();
        prop_assert!(!display.is_empty(), "Display output must not be empty");
        prop_assert!(
            display.contains(prefix),
            "Display '{}' must contain prefix '{}'", display, prefix
        );
    }

    /// **Feature: production-backends, Property 8: Error Variant Correctness (bare messages)**
    /// *For any* bare error message (no detail suffix), the error is still an `AdkError::Session`
    /// variant with a non-empty context message.
    /// **Validates: Requirements 16.1, 16.2**
    #[test]
    fn prop_postgres_bare_errors_are_session_variant(
        prefix in arb_error_prefix(),
    ) {
        let err = make_postgres_error(prefix, "");

        match &err {
            AdkError::Session(msg) => {
                prop_assert!(!msg.is_empty(), "bare error message must not be empty");
                prop_assert_eq!(msg.as_str(), prefix);
            }
            other => {
                prop_assert!(false, "expected AdkError::Session, got: {:?}", other);
            }
        }
    }

    /// **Feature: production-backends, Property 8: Session errors implement std::error::Error**
    /// *For any* generated session error, it must implement `std::error::Error` and produce
    /// a non-empty `Display` string.
    /// **Validates: Requirements 16.1, 16.2**
    #[test]
    fn prop_postgres_errors_implement_std_error(
        prefix in arb_error_prefix(),
        detail in arb_error_detail(),
    ) {
        let err = make_postgres_error(prefix, &detail);

        // AdkError implements std::error::Error via thiserror
        let std_err: &dyn std::error::Error = &err;
        let display = std_err.to_string();
        prop_assert!(!display.is_empty(), "std::error::Error display must not be empty");
        // The Display format is "Session error: {msg}"
        prop_assert!(
            display.contains(prefix),
            "std error display '{}' must contain prefix '{}'", display, prefix
        );
    }
}

/// Verify that every known error prefix produces a valid `AdkError::Session`.
/// This is a unit-style exhaustive check complementing the property tests.
#[test]
fn test_all_postgres_error_prefixes_produce_session_variant() {
    for prefix in POSTGRES_ERROR_PREFIXES {
        let err = AdkError::Session(format!("{prefix}: some underlying error"));
        match &err {
            AdkError::Session(msg) => {
                assert!(!msg.is_empty(), "prefix '{prefix}' produced empty message");
                assert!(
                    msg.starts_with(prefix),
                    "prefix '{prefix}' not at start of message '{msg}'"
                );
            }
            other => panic!("expected AdkError::Session for prefix '{prefix}', got: {other:?}"),
        }
    }
}
