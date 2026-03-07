#![cfg(feature = "redis")]

use adk_session::redis::{app_state_key, events_key, index_key, session_key, user_state_key};
use proptest::prelude::*;

/// Generate a non-empty string from `[a-zA-Z0-9_-]+` that never contains `:`.
fn arb_segment() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_-]{1,20}"
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// **Feature: production-backends, Property 3: Redis Key Generation Format**
    /// *For any* valid (app_name, user_id, session_id) triple where none contain `:`,
    /// the key functions produce the expected format patterns.
    /// **Validates: Requirements 6.1, 6.2, 6.3, 6.4**
    #[test]
    fn prop_session_key_format(
        app in arb_segment(),
        user in arb_segment(),
        session in arb_segment(),
    ) {
        let key = session_key(&app, &user, &session);
        prop_assert_eq!(&key, &format!("{app}:{user}:{session}"));

        // Key must contain exactly 2 colons (3 segments).
        let colon_count = key.chars().filter(|&c| c == ':').count();
        prop_assert_eq!(colon_count, 2, "session key should have exactly 2 colons");

        // Splitting on `:` recovers the original components.
        let parts: Vec<&str> = key.splitn(3, ':').collect();
        prop_assert_eq!(parts.len(), 3);
        prop_assert_eq!(parts[0], app.as_str());
        prop_assert_eq!(parts[1], user.as_str());
        prop_assert_eq!(parts[2], session.as_str());
    }

    #[test]
    fn prop_events_key_format(
        app in arb_segment(),
        user in arb_segment(),
        session in arb_segment(),
    ) {
        let key = events_key(&app, &user, &session);
        prop_assert_eq!(&key, &format!("{app}:{user}:{session}:events"));

        // Must end with `:events` suffix.
        prop_assert!(key.ends_with(":events"), "events key must end with :events");

        // Stripping the suffix yields the session key.
        let prefix = key.strip_suffix(":events").unwrap();
        prop_assert_eq!(prefix, session_key(&app, &user, &session));
    }

    #[test]
    fn prop_app_state_key_format(app in arb_segment()) {
        let key = app_state_key(&app);
        prop_assert_eq!(&key, &format!("app_state:{app}"));

        // Must start with `app_state:` prefix.
        prop_assert!(key.starts_with("app_state:"), "app state key must start with app_state:");

        // Stripping the prefix recovers the app name.
        let suffix = key.strip_prefix("app_state:").unwrap();
        prop_assert_eq!(suffix, app.as_str());
    }

    #[test]
    fn prop_user_state_key_format(
        app in arb_segment(),
        user in arb_segment(),
    ) {
        let key = user_state_key(&app, &user);
        prop_assert_eq!(&key, &format!("user_state:{app}:{user}"));

        // Must start with `user_state:` prefix.
        prop_assert!(key.starts_with("user_state:"), "user state key must start with user_state:");

        // Stripping the prefix and splitting recovers app and user.
        let rest = key.strip_prefix("user_state:").unwrap();
        let parts: Vec<&str> = rest.splitn(2, ':').collect();
        prop_assert_eq!(parts.len(), 2);
        prop_assert_eq!(parts[0], app.as_str());
        prop_assert_eq!(parts[1], user.as_str());
    }

    #[test]
    fn prop_index_key_format(
        app in arb_segment(),
        user in arb_segment(),
    ) {
        let key = index_key(&app, &user);
        prop_assert_eq!(&key, &format!("sessions_idx:{app}:{user}"));

        // Must start with `sessions_idx:` prefix.
        prop_assert!(key.starts_with("sessions_idx:"), "index key must start with sessions_idx:");

        // Stripping the prefix and splitting recovers app and user.
        let rest = key.strip_prefix("sessions_idx:").unwrap();
        let parts: Vec<&str> = rest.splitn(2, ':').collect();
        prop_assert_eq!(parts.len(), 2);
        prop_assert_eq!(parts[0], app.as_str());
        prop_assert_eq!(parts[1], user.as_str());
    }

    /// All five key functions produce distinct keys for the same input triple.
    #[test]
    fn prop_all_keys_are_distinct(
        app in arb_segment(),
        user in arb_segment(),
        session in arb_segment(),
    ) {
        let keys = [
            session_key(&app, &user, &session),
            events_key(&app, &user, &session),
            app_state_key(&app),
            user_state_key(&app, &user),
            index_key(&app, &user),
        ];

        // Every key must be unique.
        for i in 0..keys.len() {
            for j in (i + 1)..keys.len() {
                let ki = &keys[i];
                let kj = &keys[j];
                prop_assert_ne!(ki, kj,
                    "keys collided: {} == {}", ki, kj);
            }
        }
    }
}
