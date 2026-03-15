//! Regression tests for cross-user and cross-app session collisions using
//! typed [`AdkIdentity`] addressing.
//!
//! These tests validate that the in-memory session backend correctly isolates
//! sessions that share the same raw `session_id` but differ in `app_name` or
//! `user_id`.

use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_session::*;
use proptest::prelude::*;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn identity(app: &str, user: &str, session: &str) -> AdkIdentity {
    AdkIdentity::new(
        AppName::try_from(app).unwrap(),
        UserId::try_from(user).unwrap(),
        SessionId::try_from(session).unwrap(),
    )
}

fn create_req(app: &str, user: &str, session: &str) -> CreateRequest {
    CreateRequest {
        app_name: app.to_string(),
        user_id: user.to_string(),
        session_id: Some(session.to_string()),
        state: HashMap::new(),
    }
}

// ---------------------------------------------------------------------------
// Property 3: AdkIdentity Equality
// ---------------------------------------------------------------------------

/// Generate a non-empty identifier string suitable for typed identity fields.
fn arb_id_string() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_@:./-]{1,30}"
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: typed-identity, Property 3: AdkIdentity Equality**
    /// *For any* two `AdkIdentity` values, they are equal if and only if their
    /// `app_name`, `user_id`, and `session_id` fields are all equal.
    /// **Validates: Requirements 5.6, 11.2**
    #[test]
    fn prop_adk_identity_equality(
        app1 in arb_id_string(),
        user1 in arb_id_string(),
        sess1 in arb_id_string(),
        app2 in arb_id_string(),
        user2 in arb_id_string(),
        sess2 in arb_id_string(),
    ) {
        let id1 = identity(&app1, &user1, &sess1);
        let id2 = identity(&app2, &user2, &sess2);

        let fields_equal = app1 == app2 && user1 == user2 && sess1 == sess2;
        prop_assert_eq!(
            id1 == id2,
            fields_equal,
            "AdkIdentity equality must match iff all three fields are equal"
        );
    }
}

// ---------------------------------------------------------------------------
// Property 4: Cross-Tenant Session Isolation (in-memory backend)
// ---------------------------------------------------------------------------

/// **Feature: typed-identity, Property 4: Cross-Tenant Session Isolation**
/// For any two sessions that share the same raw `session_id` but differ in
/// `app_name` or `user_id`, typed session operations address the correct
/// session and never cross tenant boundaries.
/// **Validates: Requirements 5.6, 11.2**
mod cross_tenant_isolation {
    use super::*;

    // -- Cross-user collision: same app, same session_id, different user --

    #[tokio::test]
    async fn get_for_identity_isolates_cross_user_sessions() {
        let svc = InMemorySessionService::new();
        let shared_session = "shared-sess";

        svc.create(create_req("app", "alice", shared_session)).await.unwrap();
        svc.create(create_req("app", "bob", shared_session)).await.unwrap();

        let alice_id = identity("app", "alice", shared_session);
        let bob_id = identity("app", "bob", shared_session);

        let alice_sess = svc.get_for_identity(&alice_id).await.unwrap();
        let bob_sess = svc.get_for_identity(&bob_id).await.unwrap();

        assert_eq!(alice_sess.user_id(), "alice");
        assert_eq!(bob_sess.user_id(), "bob");
        assert_eq!(alice_sess.id(), shared_session);
        assert_eq!(bob_sess.id(), shared_session);
    }

    #[tokio::test]
    async fn append_event_for_identity_isolates_cross_user_sessions() {
        let svc = InMemorySessionService::new();
        let shared_session = "shared-sess";

        svc.create(create_req("app", "alice", shared_session)).await.unwrap();
        svc.create(create_req("app", "bob", shared_session)).await.unwrap();

        let alice_id = identity("app", "alice", shared_session);
        let bob_id = identity("app", "bob", shared_session);

        // Append event only to alice's session
        let event = Event::new("inv-alice");
        svc.append_event_for_identity(AppendEventRequest { identity: alice_id.clone(), event })
            .await
            .unwrap();

        let alice_sess = svc.get_for_identity(&alice_id).await.unwrap();
        let bob_sess = svc.get_for_identity(&bob_id).await.unwrap();

        assert_eq!(alice_sess.events().len(), 1, "alice should have 1 event");
        assert_eq!(bob_sess.events().len(), 0, "bob should have 0 events");
    }

    #[tokio::test]
    async fn delete_for_identity_isolates_cross_user_sessions() {
        let svc = InMemorySessionService::new();
        let shared_session = "shared-sess";

        svc.create(create_req("app", "alice", shared_session)).await.unwrap();
        svc.create(create_req("app", "bob", shared_session)).await.unwrap();

        let alice_id = identity("app", "alice", shared_session);
        let bob_id = identity("app", "bob", shared_session);

        // Delete only alice's session
        svc.delete_for_identity(&alice_id).await.unwrap();

        assert!(svc.get_for_identity(&alice_id).await.is_err(), "alice session should be gone");
        assert!(svc.get_for_identity(&bob_id).await.is_ok(), "bob session should still exist");
    }

    // -- Cross-app collision: different app, same user, same session_id --

    #[tokio::test]
    async fn get_for_identity_isolates_cross_app_sessions() {
        let svc = InMemorySessionService::new();
        let shared_session = "shared-sess";

        svc.create(create_req("app-a", "user", shared_session)).await.unwrap();
        svc.create(create_req("app-b", "user", shared_session)).await.unwrap();

        let id_a = identity("app-a", "user", shared_session);
        let id_b = identity("app-b", "user", shared_session);

        let sess_a = svc.get_for_identity(&id_a).await.unwrap();
        let sess_b = svc.get_for_identity(&id_b).await.unwrap();

        assert_eq!(sess_a.app_name(), "app-a");
        assert_eq!(sess_b.app_name(), "app-b");
    }

    #[tokio::test]
    async fn append_event_for_identity_isolates_cross_app_sessions() {
        let svc = InMemorySessionService::new();
        let shared_session = "shared-sess";

        svc.create(create_req("app-a", "user", shared_session)).await.unwrap();
        svc.create(create_req("app-b", "user", shared_session)).await.unwrap();

        let id_a = identity("app-a", "user", shared_session);
        let id_b = identity("app-b", "user", shared_session);

        // Append event only to app-a's session
        let event = Event::new("inv-a");
        svc.append_event_for_identity(AppendEventRequest { identity: id_a.clone(), event })
            .await
            .unwrap();

        let sess_a = svc.get_for_identity(&id_a).await.unwrap();
        let sess_b = svc.get_for_identity(&id_b).await.unwrap();

        assert_eq!(sess_a.events().len(), 1, "app-a should have 1 event");
        assert_eq!(sess_b.events().len(), 0, "app-b should have 0 events");
    }

    #[tokio::test]
    async fn delete_for_identity_isolates_cross_app_sessions() {
        let svc = InMemorySessionService::new();
        let shared_session = "shared-sess";

        svc.create(create_req("app-a", "user", shared_session)).await.unwrap();
        svc.create(create_req("app-b", "user", shared_session)).await.unwrap();

        let id_a = identity("app-a", "user", shared_session);
        let id_b = identity("app-b", "user", shared_session);

        // Delete only app-a's session
        svc.delete_for_identity(&id_a).await.unwrap();

        assert!(svc.get_for_identity(&id_a).await.is_err(), "app-a session should be gone");
        assert!(svc.get_for_identity(&id_b).await.is_ok(), "app-b session should still exist");
    }

    // -- Combined: different app AND different user, same session_id --

    #[tokio::test]
    async fn full_isolation_different_app_and_user() {
        let svc = InMemorySessionService::new();
        let shared_session = "shared-sess";

        svc.create(create_req("app-x", "alice", shared_session)).await.unwrap();
        svc.create(create_req("app-y", "bob", shared_session)).await.unwrap();

        let id_x = identity("app-x", "alice", shared_session);
        let id_y = identity("app-y", "bob", shared_session);

        // Append to app-x/alice only
        svc.append_event_for_identity(AppendEventRequest {
            identity: id_x.clone(),
            event: Event::new("inv-x"),
        })
        .await
        .unwrap();

        // Delete app-y/bob only
        svc.delete_for_identity(&id_y).await.unwrap();

        let sess_x = svc.get_for_identity(&id_x).await.unwrap();
        assert_eq!(sess_x.events().len(), 1);
        assert!(svc.get_for_identity(&id_y).await.is_err());
    }
}
