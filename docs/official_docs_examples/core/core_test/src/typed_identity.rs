//! Typed Identity doc-test — validates typed identity documentation.
//!
//! Demonstrates:
//! - Constructing `AppName`, `UserId`, `SessionId`, `InvocationId` from strings
//! - Constructing `AdkIdentity` and `ExecutionIdentity`
//! - Boundary parsing with `TryFrom<&str>` and error handling
//!
//! **Validates: Requirements 10.2, 10.3**

use adk_core::{
    AdkIdentity, AppName, ExecutionIdentity, IdentityError, InvocationId, SessionId, UserId,
};

fn main() {
    println!("=== Typed Identity Doc-Test ===\n");

    // -----------------------------------------------------------------------
    // 1. Constructing leaf identifiers from strings
    // -----------------------------------------------------------------------
    println!("1. Constructing leaf identifiers");

    let app: AppName = "weather-app".parse().unwrap();
    assert_eq!(app.as_ref(), "weather-app");
    println!("   ✓ AppName::parse()");

    let user = UserId::try_from("tenant:alice@example.com").unwrap();
    assert_eq!(user.as_ref(), "tenant:alice@example.com");
    println!("   ✓ UserId::try_from(&str) — colons and @ allowed");

    let session = SessionId::try_from("session-abc-123").unwrap();
    assert_eq!(session.as_ref(), "session-abc-123");
    println!("   ✓ SessionId::try_from(&str)");

    let invocation = InvocationId::try_from("inv-001").unwrap();
    assert_eq!(invocation.as_ref(), "inv-001");
    println!("   ✓ InvocationId::try_from(&str)");

    // TryFrom<String> also works
    let owned = String::from("my-app");
    let app2 = AppName::try_from(owned).unwrap();
    assert_eq!(app2.as_ref(), "my-app");
    println!("   ✓ AppName::try_from(String)");

    // Generation helpers for session and invocation IDs
    let generated_session = SessionId::generate();
    assert!(!generated_session.as_ref().is_empty());
    println!("   ✓ SessionId::generate() → {generated_session}");

    let generated_inv = InvocationId::generate();
    assert!(!generated_inv.as_ref().is_empty());
    println!("   ✓ InvocationId::generate() → {generated_inv}");

    // -----------------------------------------------------------------------
    // 2. Constructing AdkIdentity and ExecutionIdentity
    // -----------------------------------------------------------------------
    println!("\n2. Constructing composite identities");

    let identity = AdkIdentity::new(
        AppName::try_from("weather-app").unwrap(),
        UserId::try_from("alice").unwrap(),
        SessionId::try_from("sess-1").unwrap(),
    );
    assert_eq!(identity.app_name.as_ref(), "weather-app");
    assert_eq!(identity.user_id.as_ref(), "alice");
    assert_eq!(identity.session_id.as_ref(), "sess-1");
    println!("   ✓ AdkIdentity::new()");

    // Display is diagnostic only
    let display = format!("{identity}");
    assert!(display.contains("weather-app"));
    assert!(display.contains("alice"));
    println!("   ✓ AdkIdentity Display: {identity}");

    let exec = ExecutionIdentity {
        adk: identity.clone(),
        invocation_id: InvocationId::generate(),
        branch: "main".to_string(),
        agent_name: "planner".to_string(),
    };
    assert_eq!(exec.adk.app_name.as_ref(), "weather-app");
    assert_eq!(exec.agent_name, "planner");
    assert_eq!(exec.branch, "main");
    println!("   ✓ ExecutionIdentity constructed");

    // -----------------------------------------------------------------------
    // 3. Boundary parsing — rejecting invalid input
    // -----------------------------------------------------------------------
    println!("\n3. Boundary parsing and error handling");

    // Empty values are rejected
    let err = AppName::try_from("").unwrap_err();
    assert!(matches!(err, IdentityError::Empty { .. }));
    println!("   ✓ Empty AppName rejected: {err}");

    let err = UserId::try_from("").unwrap_err();
    assert!(matches!(err, IdentityError::Empty { .. }));
    println!("   ✓ Empty UserId rejected: {err}");

    let err = SessionId::try_from("").unwrap_err();
    assert!(matches!(err, IdentityError::Empty { .. }));
    println!("   ✓ Empty SessionId rejected: {err}");

    // Null bytes are rejected
    let err = AppName::try_from("bad\0name").unwrap_err();
    assert!(matches!(err, IdentityError::ContainsNull { .. }));
    println!("   ✓ Null byte rejected: {err}");

    // Overly long values are rejected
    let long_value = "x".repeat(513);
    let err = AppName::try_from(long_value.as_str()).unwrap_err();
    assert!(matches!(err, IdentityError::TooLong { .. }));
    println!("   ✓ Too-long value rejected: {err}");

    // Maximum length is accepted
    let max_value = "a".repeat(512);
    assert!(AppName::try_from(max_value.as_str()).is_ok());
    println!("   ✓ Max-length value (512 bytes) accepted");

    // -----------------------------------------------------------------------
    // 4. Simulated HTTP boundary parse pattern
    // -----------------------------------------------------------------------
    println!("\n4. Simulated HTTP boundary parse");

    // This is the pattern used in adk-server handlers:
    // parse user-controlled strings at the boundary, return errors early.
    fn parse_identity_at_boundary(
        raw_app: &str,
        raw_user: &str,
        raw_session: &str,
    ) -> Result<AdkIdentity, String> {
        let app_name = AppName::try_from(raw_app).map_err(|e| format!("invalid app name: {e}"))?;
        let user_id = UserId::try_from(raw_user).map_err(|e| format!("invalid user id: {e}"))?;
        let session_id =
            SessionId::try_from(raw_session).map_err(|e| format!("invalid session id: {e}"))?;
        Ok(AdkIdentity::new(app_name, user_id, session_id))
    }

    // Successful parse
    let id = parse_identity_at_boundary("my-app", "user-1", "sess-1").unwrap();
    assert_eq!(id.app_name.as_ref(), "my-app");
    println!("   ✓ Valid boundary parse succeeded");

    // Failed parse — descriptive error
    let err = parse_identity_at_boundary("", "user-1", "sess-1").unwrap_err();
    assert!(err.contains("invalid app name"));
    println!("   ✓ Invalid boundary parse returned error: {err}");

    let err = parse_identity_at_boundary("app", "", "sess-1").unwrap_err();
    assert!(err.contains("invalid user id"));
    println!("   ✓ Invalid user at boundary returned error: {err}");

    // -----------------------------------------------------------------------
    // 5. Serde round-trip (transparent serialization)
    // -----------------------------------------------------------------------
    println!("\n5. Serde round-trip");

    let app = AppName::try_from("my-app").unwrap();
    let json = serde_json::to_string(&app).unwrap();
    assert_eq!(json, "\"my-app\""); // transparent — plain string
    let deserialized: AppName = serde_json::from_str(&json).unwrap();
    assert_eq!(app, deserialized);
    println!("   ✓ AppName serde round-trip (transparent)");

    let identity = AdkIdentity::new(
        AppName::try_from("app-1").unwrap(),
        UserId::try_from("user-1").unwrap(),
        SessionId::try_from("sess-1").unwrap(),
    );
    let json = serde_json::to_string(&identity).unwrap();
    let deserialized: AdkIdentity = serde_json::from_str(&json).unwrap();
    assert_eq!(identity, deserialized);
    println!("   ✓ AdkIdentity serde round-trip");

    println!("\n=== All typed identity tests passed! ===");
}
