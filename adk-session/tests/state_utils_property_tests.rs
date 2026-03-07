use adk_session::{
    KEY_PREFIX_APP, KEY_PREFIX_TEMP, KEY_PREFIX_USER, extract_state_deltas, merge_states,
};
use proptest::prelude::*;
use serde_json::Value;
use std::collections::HashMap;

/// Generate an arbitrary JSON value (limited depth for performance).
fn arb_json_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| Value::Number(n.into())),
        "[a-zA-Z0-9 _-]{0,20}".prop_map(|s| Value::String(s)),
    ]
}

/// Generate a key with a random prefix category.
fn arb_prefixed_key() -> impl Strategy<Value = String> {
    let base_key = "[a-zA-Z_][a-zA-Z0-9_]{0,10}";
    prop_oneof![
        base_key.prop_map(|k| format!("{KEY_PREFIX_APP}{k}")),
        base_key.prop_map(|k| format!("{KEY_PREFIX_USER}{k}")),
        base_key.prop_map(|k| format!("{KEY_PREFIX_TEMP}{k}")),
        base_key, // session-level (no prefix)
    ]
}

/// Generate a state map with mixed prefix keys.
fn arb_state_map() -> impl Strategy<Value = HashMap<String, Value>> {
    prop::collection::hash_map(arb_prefixed_key(), arb_json_value(), 0..30)
}

/// Generate a state map with no `temp:` keys (for round-trip testing).
fn arb_state_map_no_temp() -> impl Strategy<Value = HashMap<String, Value>> {
    let base_key = "[a-zA-Z_][a-zA-Z0-9_]{0,10}";
    let non_temp_key = prop_oneof![
        base_key.prop_map(|k| format!("{KEY_PREFIX_APP}{k}")),
        base_key.prop_map(|k| format!("{KEY_PREFIX_USER}{k}")),
        base_key, // session-level
    ];
    prop::collection::hash_map(non_temp_key, arb_json_value(), 0..30)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// **Feature: production-backends, Property 1: State Delta Extraction Partitions by Prefix**
    /// *For any* HashMap<String, Value>, `extract_state_deltas()` places `app:` keys in app tier
    /// (stripped), `user:` keys in user tier (stripped), `temp:` keys in none, and remaining keys
    /// in session tier. Union of all tiers plus dropped temp keys accounts for every input key.
    /// **Validates: Requirements 3.1, 3.5, 7.1, 7.5**
    #[test]
    fn prop_state_delta_extraction_partitions_by_prefix(input in arb_state_map()) {
        let (app, user, session) = extract_state_deltas(&input);

        for (key, value) in &input {
            if let Some(stripped) = key.strip_prefix(KEY_PREFIX_APP) {
                // app: keys land in app tier with prefix stripped
                prop_assert_eq!(app.get(stripped), Some(value),
                    "app: key '{}' missing from app tier", key);
                prop_assert!(!session.contains_key(key),
                    "app: key should not appear in session tier with original key");
            } else if let Some(stripped) = key.strip_prefix(KEY_PREFIX_USER) {
                // user: keys land in user tier with prefix stripped
                prop_assert_eq!(user.get(stripped), Some(value),
                    "user: key '{}' missing from user tier", key);
                prop_assert!(!session.contains_key(key),
                    "user: key should not appear in session tier with original key");
            } else if key.starts_with(KEY_PREFIX_TEMP) {
                // temp: keys are dropped entirely
                prop_assert!(!app.contains_key(key), "temp: key in app tier");
                prop_assert!(!user.contains_key(key), "temp: key in user tier");
                prop_assert!(!session.contains_key(key), "temp: key in session tier");
            } else {
                // remaining keys go to session tier unchanged
                prop_assert_eq!(session.get(key), Some(value),
                    "session key '{}' missing from session tier", key);
            }
        }

        // Verify total count: app + user + session + temp == input
        let temp_count = input.keys().filter(|k| k.starts_with(KEY_PREFIX_TEMP)).count();
        prop_assert_eq!(
            app.len() + user.len() + session.len() + temp_count,
            input.len(),
            "partition sizes don't sum to input size"
        );
    }

    /// **Feature: production-backends, Property 2: State Delta Round-Trip**
    /// *For any* HashMap<String, Value> with no `temp:` prefixed keys, `extract_state_deltas()`
    /// followed by `merge_states()` produces a map equal to the original.
    /// **Validates: Requirements 3.3, 7.3, 17.1, 17.2**
    #[test]
    fn prop_state_delta_round_trip(input in arb_state_map_no_temp()) {
        let (app, user, session) = extract_state_deltas(&input);
        let merged = merge_states(&app, &user, &session);

        prop_assert_eq!(
            &merged, &input,
            "round-trip failed: extract then merge should reproduce the original map"
        );
    }
}
