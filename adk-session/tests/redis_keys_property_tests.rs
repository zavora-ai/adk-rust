#![cfg(feature = "redis")]

use adk_session::redis::{app_state_key, events_key, index_key, session_key, user_state_key};
use proptest::prelude::*;

/// Generate a non-empty string from `[a-zA-Z0-9_-]+` that never contains `:`.
///
/// Key-family prefixes are excluded: an app with one of those names has its
/// first byte encoded, which `reserved_app_names_do_not_collide_with_other_key_families`
/// covers.
fn arb_segment() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_-]{1,20}".prop_filter("reserved key prefix", |s| {
        !matches!(s.as_str(), "app_state" | "user_state" | "sessions_idx" | "session_lookup")
    })
}

/// Generate an identifier that may contain the `:` delimiter and `%`.
fn arb_delimited_segment() -> impl Strategy<Value = String> {
    "[a-z:%]{1,8}"
}

#[test]
fn colon_in_identifiers_does_not_collide() {
    assert_ne!(session_key("a:b", "c", "s"), session_key("a", "b:c", "s"));
    assert_ne!(user_state_key("a:b", "c"), user_state_key("a", "b:c"));
    assert_ne!(index_key("a:b", "c"), index_key("a", "b:c"));
    // A session id ending in `:events` must not alias another session's event set.
    assert_ne!(session_key("a", "u", "s:events"), events_key("a", "u", "s"));
    assert_eq!(
        [session_key("a:b", "100%", "s"), app_state_key("a:b")],
        ["a%3Ab:100%25:s".to_string(), "app_state:a%3Ab".to_string()]
    );
}

#[test]
fn reserved_app_names_do_not_collide_with_other_key_families() {
    assert_ne!(session_key("user_state", "app", "alice"), user_state_key("app", "alice"));
    assert_ne!(session_key("sessions_idx", "app", "alice"), index_key("app", "alice"));
    assert_eq!(session_key("user_state", "app", "alice"), "%75ser_state:app:alice");
    // State keys embed the app name after a prefix, so it is not escaped there.
    assert_eq!(user_state_key("user_state", "alice"), "user_state:user_state:alice");
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

    /// With `:` and `%` allowed in identifiers, two keys are equal exactly when
    /// they belong to the same family and every component they encode is equal.
    #[test]
    fn prop_delimited_identifiers_never_collide(
        a in (arb_delimited_segment(), arb_delimited_segment(), arb_delimited_segment()),
        b in (arb_delimited_segment(), arb_delimited_segment(), arb_delimited_segment()),
    ) {
        let same_app = a.0 == b.0;
        let same_user = same_app && a.1 == b.1;
        let same_session = same_user && a.2 == b.2;

        prop_assert_eq!(session_key(&a.0, &a.1, &a.2) == session_key(&b.0, &b.1, &b.2), same_session);
        prop_assert_eq!(events_key(&a.0, &a.1, &a.2) == events_key(&b.0, &b.1, &b.2), same_session);
        prop_assert_eq!(user_state_key(&a.0, &a.1) == user_state_key(&b.0, &b.1), same_user);
        prop_assert_eq!(index_key(&a.0, &a.1) == index_key(&b.0, &b.1), same_user);
        prop_assert_eq!(app_state_key(&a.0) == app_state_key(&b.0), same_app);

        let family = |(app, user, session): &(String, String, String)| {
            [
                session_key(app, user, session),
                events_key(app, user, session),
                app_state_key(app),
                user_state_key(app, user),
                index_key(app, user),
            ]
        };
        let (keys_a, keys_b) = (family(&a), family(&b));
        for (i, ka) in keys_a.iter().enumerate() {
            for (j, kb) in keys_b.iter().enumerate() {
                if i != j {
                    prop_assert_ne!(ka, kb, "different key families collided");
                }
            }
        }
    }
}
