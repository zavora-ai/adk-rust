//! Tests for typed identity parsing at ingress boundaries.
//!
//! These tests verify that the identity parsing logic used by `adk-server`
//! handlers correctly rejects invalid inputs and accepts valid ones, including
//! values with special characters like `:`, `@`, and `/`.
//!
//! **Validates: Requirements 7.2, 7.3, 11.3**

use adk_core::identity::{AppName, IdentityError, MAX_ID_LEN, SessionId, UserId};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Helpers — simulate the boundary parse pattern used in handlers
// ---------------------------------------------------------------------------

/// Simulates the boundary parsing pattern from server handlers.
/// Returns `Ok(())` when all three identity components parse successfully,
/// or `Err(String)` with a descriptive message (as a handler would return
/// in a 400 Bad Request body).
fn parse_identity_at_boundary(
    app_name: &str,
    user_id: &str,
    session_id: &str,
) -> Result<(AppName, UserId, SessionId), String> {
    let app = AppName::try_from(app_name).map_err(|e| format!("invalid app_name: {e}"))?;
    let user = UserId::try_from(user_id).map_err(|e| format!("invalid user_id: {e}"))?;
    let session =
        SessionId::try_from(session_id).map_err(|e| format!("invalid session_id: {e}"))?;
    Ok((app, user, session))
}

// ---------------------------------------------------------------------------
// Unit tests — invalid ID rejection
// ---------------------------------------------------------------------------

#[test]
fn test_empty_app_name_rejected() {
    let result = AppName::try_from("");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), IdentityError::Empty { kind: "AppName" });
}

#[test]
fn test_empty_user_id_rejected() {
    let result = UserId::try_from("");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), IdentityError::Empty { kind: "UserId" });
}

#[test]
fn test_empty_session_id_rejected() {
    let result = SessionId::try_from("");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), IdentityError::Empty { kind: "SessionId" });
}

#[test]
fn test_null_byte_app_name_rejected() {
    let result = AppName::try_from("my\0app");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), IdentityError::ContainsNull { kind: "AppName" });
}

#[test]
fn test_null_byte_user_id_rejected() {
    let result = UserId::try_from("user\0id");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), IdentityError::ContainsNull { kind: "UserId" });
}

#[test]
fn test_null_byte_session_id_rejected() {
    let result = SessionId::try_from("sess\0ion");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), IdentityError::ContainsNull { kind: "SessionId" });
}

#[test]
fn test_too_long_rejected() {
    let long = "a".repeat(MAX_ID_LEN + 1);
    assert!(AppName::try_from(long.as_str()).is_err());
    assert!(UserId::try_from(long.as_str()).is_err());
    assert!(SessionId::try_from(long.as_str()).is_err());
}

// ---------------------------------------------------------------------------
// Unit tests — boundary parse helper rejects invalid components
// ---------------------------------------------------------------------------

#[test]
fn test_boundary_parse_rejects_empty_app_name() {
    let err = parse_identity_at_boundary("", "user1", "session1").unwrap_err();
    assert!(err.contains("invalid app_name"), "got: {err}");
}

#[test]
fn test_boundary_parse_rejects_empty_user_id() {
    let err = parse_identity_at_boundary("app1", "", "session1").unwrap_err();
    assert!(err.contains("invalid user_id"), "got: {err}");
}

#[test]
fn test_boundary_parse_rejects_empty_session_id() {
    let err = parse_identity_at_boundary("app1", "user1", "").unwrap_err();
    assert!(err.contains("invalid session_id"), "got: {err}");
}

#[test]
fn test_boundary_parse_rejects_null_byte_in_any_field() {
    assert!(parse_identity_at_boundary("app\0x", "user1", "session1").is_err());
    assert!(parse_identity_at_boundary("app1", "user\0x", "session1").is_err());
    assert!(parse_identity_at_boundary("app1", "user1", "sess\0x").is_err());
}

// ---------------------------------------------------------------------------
// Unit tests — valid IDs with special characters accepted
// ---------------------------------------------------------------------------

#[test]
fn test_valid_simple_ids_accepted() {
    let result = parse_identity_at_boundary("weather-app", "alice", "abc-123");
    assert!(result.is_ok());
    let (app, user, session) = result.unwrap();
    assert_eq!(app.as_ref(), "weather-app");
    assert_eq!(user.as_ref(), "alice");
    assert_eq!(session.as_ref(), "abc-123");
}

#[test]
fn test_colon_in_user_id_accepted() {
    let result = parse_identity_at_boundary("my-app", "tenant:alice@example.com", "s1");
    assert!(result.is_ok());
    assert_eq!(result.unwrap().1.as_ref(), "tenant:alice@example.com");
}

#[test]
fn test_slash_in_app_name_accepted() {
    let result = parse_identity_at_boundary("org/weather-app", "user1", "s1");
    assert!(result.is_ok());
    assert_eq!(result.unwrap().0.as_ref(), "org/weather-app");
}

#[test]
fn test_at_sign_in_user_id_accepted() {
    let result = parse_identity_at_boundary("app", "user@domain.com", "s1");
    assert!(result.is_ok());
    assert_eq!(result.unwrap().1.as_ref(), "user@domain.com");
}

#[test]
fn test_uuid_session_id_accepted() {
    let result = parse_identity_at_boundary("app", "user", "550e8400-e29b-41d4-a716-446655440000");
    assert!(result.is_ok());
}

#[test]
fn test_max_length_boundary_accepted() {
    let max_id = "x".repeat(MAX_ID_LEN);
    let result = parse_identity_at_boundary(&max_id, &max_id, &max_id);
    assert!(result.is_ok());
}

#[test]
fn test_error_messages_are_descriptive() {
    let err = parse_identity_at_boundary("", "user", "session").unwrap_err();
    assert!(
        err.contains("must not be empty"),
        "error should contain descriptive message, got: {err}"
    );

    let err = parse_identity_at_boundary("app", "u\0ser", "session").unwrap_err();
    assert!(
        err.contains("must not contain null bytes"),
        "error should contain descriptive message, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Property tests — boundary parsing
// ---------------------------------------------------------------------------

fn arb_valid_id() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9:@/_\\-\\.\\+\\|~]{1,128}"
}

fn arb_null_containing() -> impl Strategy<Value = String> {
    ("[a-z]{0,10}", "[a-z]{0,10}").prop_map(|(prefix, suffix)| format!("{prefix}\0{suffix}"))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: typed-identity, Task 5.4: Boundary Parse Acceptance**
    /// *For any* valid identity triple, boundary parsing succeeds and preserves
    /// the original string values.
    /// **Validates: Requirements 7.2, 7.3, 11.3**
    #[test]
    fn prop_valid_identity_triple_accepted(
        app in arb_valid_id(),
        user in arb_valid_id(),
        session in arb_valid_id(),
    ) {
        let result = parse_identity_at_boundary(&app, &user, &session);
        prop_assert!(result.is_ok(), "valid triple should parse: app={app:?}, user={user:?}, session={session:?}");
        let (a, u, s) = result.unwrap();
        prop_assert_eq!(a.as_ref(), app.as_str());
        prop_assert_eq!(u.as_ref(), user.as_str());
        prop_assert_eq!(s.as_ref(), session.as_str());
    }

    /// **Feature: typed-identity, Task 5.4: Empty ID Rejection at Boundary**
    /// *For any* identity triple where one component is empty, boundary parsing
    /// fails with a descriptive error.
    /// **Validates: Requirements 7.2, 7.3, 11.3**
    #[test]
    fn prop_empty_component_rejected_at_boundary(
        valid1 in arb_valid_id(),
        valid2 in arb_valid_id(),
        position in 0..3u8,
    ) {
        let (app, user, session) = match position {
            0 => (String::new(), valid1, valid2),
            1 => (valid1, String::new(), valid2),
            _ => (valid1, valid2, String::new()),
        };
        let result = parse_identity_at_boundary(&app, &user, &session);
        prop_assert!(result.is_err(), "empty component at position {position} should be rejected");
        let err = result.unwrap_err();
        prop_assert!(err.contains("must not be empty"), "error should be descriptive: {err}");
    }

    /// **Feature: typed-identity, Task 5.4: Null-Byte Rejection at Boundary**
    /// *For any* identity triple where one component contains a null byte,
    /// boundary parsing fails with a descriptive error.
    /// **Validates: Requirements 7.2, 7.3, 11.3**
    #[test]
    fn prop_null_byte_rejected_at_boundary(
        valid1 in arb_valid_id(),
        valid2 in arb_valid_id(),
        null_val in arb_null_containing(),
        position in 0..3u8,
    ) {
        let (app, user, session) = match position {
            0 => (null_val, valid1, valid2),
            1 => (valid1, null_val, valid2),
            _ => (valid1, valid2, null_val),
        };
        let result = parse_identity_at_boundary(&app, &user, &session);
        prop_assert!(result.is_err(), "null-byte component at position {position} should be rejected");
        let err = result.unwrap_err();
        prop_assert!(err.contains("must not contain null bytes"), "error should be descriptive: {err}");
    }
}
