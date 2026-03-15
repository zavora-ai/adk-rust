//! Tests for typed identity parsing at adk-studio ingress boundaries.
//!
//! These tests verify that the `SessionId` validation logic used by
//! `adk-studio` handlers (stream_handler, kill_session, resume_session)
//! correctly rejects invalid inputs and accepts valid ones.
//!
//! **Validates: Requirements 7.2, 7.3, 11.3**

use adk_core::identity::{IdentityError, MAX_ID_LEN, SessionId};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Helper — simulates the boundary parse pattern from adk-studio handlers
// ---------------------------------------------------------------------------

/// Simulates the `SessionId::try_from` validation pattern used in
/// `stream_handler`, `kill_session`, and `resume_session`.
fn validate_session_id_at_boundary(session_id: &str) -> Result<SessionId, String> {
    SessionId::try_from(session_id).map_err(|e| format!("invalid session_id: {e}"))
}

// ---------------------------------------------------------------------------
// Unit tests — invalid session IDs rejected
// ---------------------------------------------------------------------------

#[test]
fn test_empty_session_id_rejected() {
    let result = validate_session_id_at_boundary("");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("must not be empty"));
}

#[test]
fn test_null_byte_session_id_rejected() {
    let result = validate_session_id_at_boundary("sess\0ion");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("must not contain null bytes"));
}

#[test]
fn test_too_long_session_id_rejected() {
    let long = "a".repeat(MAX_ID_LEN + 1);
    let result = validate_session_id_at_boundary(&long);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("exceeds maximum length"));
}

// ---------------------------------------------------------------------------
// Unit tests — direct IdentityError variant checks
// ---------------------------------------------------------------------------

#[test]
fn test_empty_returns_correct_error_variant() {
    let err = SessionId::try_from("").unwrap_err();
    assert_eq!(err, IdentityError::Empty { kind: "SessionId" });
}

#[test]
fn test_null_byte_returns_correct_error_variant() {
    let err = SessionId::try_from("a\0b").unwrap_err();
    assert_eq!(err, IdentityError::ContainsNull { kind: "SessionId" });
}

#[test]
fn test_too_long_returns_correct_error_variant() {
    let long = "x".repeat(MAX_ID_LEN + 1);
    let err = SessionId::try_from(long.as_str()).unwrap_err();
    assert_eq!(err, IdentityError::TooLong { kind: "SessionId", max: MAX_ID_LEN });
}

// ---------------------------------------------------------------------------
// Unit tests — valid session IDs accepted
// ---------------------------------------------------------------------------

#[test]
fn test_uuid_session_id_accepted() {
    let result = validate_session_id_at_boundary("550e8400-e29b-41d4-a716-446655440000");
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_ref(), "550e8400-e29b-41d4-a716-446655440000");
}

#[test]
fn test_simple_session_id_accepted() {
    let result = validate_session_id_at_boundary("my-session-123");
    assert!(result.is_ok());
}

#[test]
fn test_session_id_with_colon_accepted() {
    let result = validate_session_id_at_boundary("tenant:session:abc");
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_ref(), "tenant:session:abc");
}

#[test]
fn test_session_id_with_slash_accepted() {
    let result = validate_session_id_at_boundary("org/project/session");
    assert!(result.is_ok());
}

#[test]
fn test_session_id_with_at_sign_accepted() {
    let result = validate_session_id_at_boundary("session@host");
    assert!(result.is_ok());
}

#[test]
fn test_max_length_session_id_accepted() {
    let max_id = "s".repeat(MAX_ID_LEN);
    let result = validate_session_id_at_boundary(&max_id);
    assert!(result.is_ok());
}

#[test]
fn test_error_message_format_matches_handler_pattern() {
    // Verify the error message format matches what handlers produce
    let err = validate_session_id_at_boundary("").unwrap_err();
    assert!(
        err.starts_with("invalid session_id:"),
        "error should match handler format 'invalid session_id: ...', got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Property tests — session ID boundary validation
// ---------------------------------------------------------------------------

fn arb_valid_session_id() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9:@/_\\-\\.\\+\\|~]{1,128}"
}

fn arb_null_containing() -> impl Strategy<Value = String> {
    ("[a-z]{0,10}", "[a-z]{0,10}").prop_map(|(prefix, suffix)| format!("{prefix}\0{suffix}"))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: typed-identity, Task 5.4: Studio Session ID Acceptance**
    /// *For any* valid session ID string, boundary validation succeeds and
    /// preserves the original value.
    /// **Validates: Requirements 7.2, 7.3, 11.3**
    #[test]
    fn prop_valid_session_id_accepted(s in arb_valid_session_id()) {
        let result = validate_session_id_at_boundary(&s);
        prop_assert!(result.is_ok(), "valid session_id should be accepted: {s:?}");
        let parsed = result.unwrap();
        prop_assert_eq!(parsed.as_ref(), s.as_str());
    }

    /// **Feature: typed-identity, Task 5.4: Studio Empty Session ID Rejection**
    /// *For any* empty string, boundary validation fails with a descriptive error.
    /// **Validates: Requirements 7.2, 7.3, 11.3**
    #[test]
    fn prop_empty_session_id_rejected(_ in Just(())) {
        let result = validate_session_id_at_boundary("");
        prop_assert!(result.is_err());
        let err = result.unwrap_err();
        prop_assert!(err.contains("must not be empty"), "error should be descriptive: {err}");
    }

    /// **Feature: typed-identity, Task 5.4: Studio Null-Byte Session ID Rejection**
    /// *For any* string containing a null byte, boundary validation fails with
    /// a descriptive error.
    /// **Validates: Requirements 7.2, 7.3, 11.3**
    #[test]
    fn prop_null_byte_session_id_rejected(s in arb_null_containing()) {
        let result = validate_session_id_at_boundary(&s);
        prop_assert!(result.is_err());
        let err = result.unwrap_err();
        prop_assert!(err.contains("must not contain null bytes"), "error should be descriptive: {err}");
    }

    /// **Feature: typed-identity, Task 5.4: Studio Too-Long Session ID Rejection**
    /// *For any* string exceeding MAX_ID_LEN, boundary validation fails.
    /// **Validates: Requirements 7.2, 11.3**
    #[test]
    fn prop_too_long_session_id_rejected(extra in 1..256usize) {
        let long = "a".repeat(MAX_ID_LEN + extra);
        let result = validate_session_id_at_boundary(&long);
        prop_assert!(result.is_err());
        let err = result.unwrap_err();
        prop_assert!(err.contains("exceeds maximum length"), "error should be descriptive: {err}");
    }
}
