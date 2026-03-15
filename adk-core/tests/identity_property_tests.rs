//! Property-based tests for typed identity parsing and serde round-trips.
//!
//! **Feature: typed-identity**
//! - Property 1: Typed Identifier Parse and Serde Round-Trip
//! - Property 2: Invalid Identifier Rejection
//!
//! **Validates: Requirements 1.4, 1.6, 9.1, 11.1**

use adk_core::identity::{
    AdkIdentity, AppName, ExecutionIdentity, IdentityError, InvocationId, MAX_ID_LEN, SessionId,
    UserId,
};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

/// Generate a valid identifier string: non-empty, no null bytes, length 1..=512.
/// Includes special characters like `:`, `@`, `/` that must be accepted.
fn arb_valid_id() -> impl Strategy<Value = String> {
    // Characters that are explicitly allowed per Requirement 1.6
    let charset = "[a-zA-Z0-9:@/_\\-\\.\\+\\|~]{1,128}";
    charset.prop_map(|s| s)
}

/// Generate a valid identifier at the maximum allowed length boundary.
fn arb_max_len_id() -> impl Strategy<Value = String> {
    (1..=MAX_ID_LEN).prop_map(|len| "x".repeat(len))
}

/// Generate an empty string (always invalid).
fn arb_empty() -> impl Strategy<Value = String> {
    Just(String::new())
}

/// Generate a string containing at least one null byte (always invalid).
fn arb_null_containing() -> impl Strategy<Value = String> {
    ("[a-z]{0,10}", "[a-z]{0,10}").prop_map(|(prefix, suffix)| format!("{prefix}\0{suffix}"))
}

// ---------------------------------------------------------------------------
// Property 1: Typed Identifier Parse and Serde Round-Trip
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: typed-identity, Property 1: Typed Identifier Parse and Serde Round-Trip**
    /// *For any* valid AppName, parsing then serializing then deserializing
    /// produces an equivalent typed identifier.
    /// **Validates: Requirements 1.4, 9.1**
    #[test]
    fn prop_app_name_serde_round_trip(s in arb_valid_id()) {
        let parsed = AppName::try_from(s.as_str()).unwrap();
        let json = serde_json::to_string(&parsed).unwrap();
        let deserialized: AppName = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&parsed, &deserialized);
        prop_assert_eq!(parsed.as_ref(), s.as_str());
        // Transparent serde: JSON is just a quoted string
        prop_assert_eq!(&json, &format!("\"{}\"", s));
    }

    /// **Feature: typed-identity, Property 1: Typed Identifier Parse and Serde Round-Trip**
    /// *For any* valid UserId, parsing then serializing then deserializing
    /// produces an equivalent typed identifier.
    /// **Validates: Requirements 1.4, 9.1**
    #[test]
    fn prop_user_id_serde_round_trip(s in arb_valid_id()) {
        let parsed = UserId::try_from(s.as_str()).unwrap();
        let json = serde_json::to_string(&parsed).unwrap();
        let deserialized: UserId = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&parsed, &deserialized);
        prop_assert_eq!(parsed.as_ref(), s.as_str());
    }

    /// **Feature: typed-identity, Property 1: Typed Identifier Parse and Serde Round-Trip**
    /// *For any* valid SessionId, parsing then serializing then deserializing
    /// produces an equivalent typed identifier.
    /// **Validates: Requirements 1.4, 9.1**
    #[test]
    fn prop_session_id_serde_round_trip(s in arb_valid_id()) {
        let parsed = SessionId::try_from(s.as_str()).unwrap();
        let json = serde_json::to_string(&parsed).unwrap();
        let deserialized: SessionId = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&parsed, &deserialized);
        prop_assert_eq!(parsed.as_ref(), s.as_str());
    }

    /// **Feature: typed-identity, Property 1: Typed Identifier Parse and Serde Round-Trip**
    /// *For any* valid InvocationId, parsing then serializing then deserializing
    /// produces an equivalent typed identifier.
    /// **Validates: Requirements 1.4, 9.1**
    #[test]
    fn prop_invocation_id_serde_round_trip(s in arb_valid_id()) {
        let parsed = InvocationId::try_from(s.as_str()).unwrap();
        let json = serde_json::to_string(&parsed).unwrap();
        let deserialized: InvocationId = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&parsed, &deserialized);
        prop_assert_eq!(parsed.as_ref(), s.as_str());
    }

    /// **Feature: typed-identity, Property 1: Typed Identifier Parse and Serde Round-Trip**
    /// *For any* valid AdkIdentity, serializing then deserializing produces
    /// an equivalent composite identity.
    /// **Validates: Requirements 1.4, 9.1**
    #[test]
    fn prop_adk_identity_serde_round_trip(
        app in arb_valid_id(),
        user in arb_valid_id(),
        session in arb_valid_id(),
    ) {
        let identity = AdkIdentity::new(
            AppName::try_from(app.as_str()).unwrap(),
            UserId::try_from(user.as_str()).unwrap(),
            SessionId::try_from(session.as_str()).unwrap(),
        );
        let json = serde_json::to_string(&identity).unwrap();
        let deserialized: AdkIdentity = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&identity, &deserialized);
    }

    /// **Feature: typed-identity, Property 1: Typed Identifier Parse and Serde Round-Trip**
    /// *For any* valid ExecutionIdentity, serializing then deserializing produces
    /// an equivalent execution identity.
    /// **Validates: Requirements 1.4, 9.1**
    #[test]
    fn prop_execution_identity_serde_round_trip(
        app in arb_valid_id(),
        user in arb_valid_id(),
        session in arb_valid_id(),
        invocation in arb_valid_id(),
        branch in "[a-z]{0,20}",
        agent in "[a-z_]{1,20}",
    ) {
        let exec = ExecutionIdentity {
            adk: AdkIdentity::new(
                AppName::try_from(app.as_str()).unwrap(),
                UserId::try_from(user.as_str()).unwrap(),
                SessionId::try_from(session.as_str()).unwrap(),
            ),
            invocation_id: InvocationId::try_from(invocation.as_str()).unwrap(),
            branch,
            agent_name: agent,
        };
        let json = serde_json::to_string(&exec).unwrap();
        let deserialized: ExecutionIdentity = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&exec, &deserialized);
    }

    /// **Feature: typed-identity, Property 1: Typed Identifier Parse and Serde Round-Trip**
    /// *For any* valid identifier at the max length boundary, parsing succeeds
    /// and round-trips correctly.
    /// **Validates: Requirements 1.4, 1.6, 9.1**
    #[test]
    fn prop_max_length_boundary_round_trip(s in arb_max_len_id()) {
        let parsed = AppName::try_from(s.as_str()).unwrap();
        let json = serde_json::to_string(&parsed).unwrap();
        let deserialized: AppName = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&parsed, &deserialized);
    }
}

// ---------------------------------------------------------------------------
// Property 2: Invalid Identifier Rejection
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: typed-identity, Property 2: Invalid Identifier Rejection**
    /// *For any* empty string, parsing into any typed identifier fails with
    /// `IdentityError::Empty` and never panics.
    /// **Validates: Requirements 1.4, 11.1**
    #[test]
    fn prop_empty_rejected_all_types(_ in arb_empty()) {
        let app_err = AppName::try_from("");
        let user_err = UserId::try_from("");
        let session_err = SessionId::try_from("");
        let inv_err = InvocationId::try_from("");

        prop_assert!(app_err.is_err());
        prop_assert!(user_err.is_err());
        prop_assert!(session_err.is_err());
        prop_assert!(inv_err.is_err());

        prop_assert_eq!(app_err.unwrap_err(), IdentityError::Empty { kind: "AppName" });
        prop_assert_eq!(user_err.unwrap_err(), IdentityError::Empty { kind: "UserId" });
        prop_assert_eq!(session_err.unwrap_err(), IdentityError::Empty { kind: "SessionId" });
        prop_assert_eq!(inv_err.unwrap_err(), IdentityError::Empty { kind: "InvocationId" });
    }

    /// **Feature: typed-identity, Property 2: Invalid Identifier Rejection**
    /// *For any* string containing a null byte, parsing into any typed identifier
    /// fails with `IdentityError::ContainsNull` and never panics.
    /// **Validates: Requirements 1.4, 11.1**
    #[test]
    fn prop_null_byte_rejected_all_types(s in arb_null_containing()) {
        let app_err = AppName::try_from(s.as_str());
        let user_err = UserId::try_from(s.as_str());
        let session_err = SessionId::try_from(s.as_str());
        let inv_err = InvocationId::try_from(s.as_str());

        prop_assert!(app_err.is_err());
        prop_assert!(user_err.is_err());
        prop_assert!(session_err.is_err());
        prop_assert!(inv_err.is_err());

        prop_assert_eq!(app_err.unwrap_err(), IdentityError::ContainsNull { kind: "AppName" });
        prop_assert_eq!(user_err.unwrap_err(), IdentityError::ContainsNull { kind: "UserId" });
        prop_assert_eq!(session_err.unwrap_err(), IdentityError::ContainsNull { kind: "SessionId" });
        prop_assert_eq!(inv_err.unwrap_err(), IdentityError::ContainsNull { kind: "InvocationId" });
    }

    /// **Feature: typed-identity, Property 2: Invalid Identifier Rejection**
    /// *For any* string exceeding MAX_ID_LEN, parsing into any typed identifier
    /// fails with `IdentityError::TooLong` and never panics.
    /// **Validates: Requirements 1.4, 11.1**
    #[test]
    fn prop_too_long_rejected_all_types(extra in 1..256usize) {
        let s = "a".repeat(MAX_ID_LEN + extra);

        let app_err = AppName::try_from(s.as_str());
        let user_err = UserId::try_from(s.as_str());
        let session_err = SessionId::try_from(s.as_str());
        let inv_err = InvocationId::try_from(s.as_str());

        prop_assert!(app_err.is_err());
        prop_assert!(user_err.is_err());
        prop_assert!(session_err.is_err());
        prop_assert!(inv_err.is_err());

        prop_assert_eq!(app_err.unwrap_err(), IdentityError::TooLong { kind: "AppName", max: MAX_ID_LEN });
        prop_assert_eq!(user_err.unwrap_err(), IdentityError::TooLong { kind: "UserId", max: MAX_ID_LEN });
        prop_assert_eq!(session_err.unwrap_err(), IdentityError::TooLong { kind: "SessionId", max: MAX_ID_LEN });
        prop_assert_eq!(inv_err.unwrap_err(), IdentityError::TooLong { kind: "InvocationId", max: MAX_ID_LEN });
    }

    /// **Feature: typed-identity, Property 1 & 2: Special Characters Acceptance**
    /// *For any* valid identifier containing `:`, `@`, or `/`, parsing succeeds
    /// and the value is preserved through serde round-trip.
    /// **Validates: Requirements 1.6, 9.1**
    #[test]
    fn prop_special_chars_accepted(
        prefix in "[a-z]{1,10}",
        sep in prop::sample::select(vec![':', '@', '/']),
        suffix in "[a-z]{1,10}",
    ) {
        let s = format!("{prefix}{sep}{suffix}");
        let parsed = AppName::try_from(s.as_str()).unwrap();
        prop_assert_eq!(parsed.as_ref(), s.as_str());

        let json = serde_json::to_string(&parsed).unwrap();
        let deserialized: AppName = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&parsed, &deserialized);
    }
}
