//! Property tests for delta checkpoint round-trip correctness.
//!
//! **Feature: runtime-reliability-sprint, Property 5: Delta Round-Trip**
//! *For any* state S₁ and S₂, `Diff::apply(S₁, Diff::diff(S₁, S₂)) == S₂`.
//! **Validates: Requirements 16.2, 16.3**

#![cfg(feature = "delta-checkpoint")]

use std::collections::HashMap;

use adk_graph::delta::Diff;
use proptest::prelude::*;
use serde_json::Value;

// ── Generators ────────────────────────────────────────────────────────

/// Generate a random JSON value suitable for vec/map entries.
fn arb_json_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<f64>().prop_filter("must be finite", |f| f.is_finite()).prop_map(Value::from),
        "[a-zA-Z0-9 ]{0,20}".prop_map(Value::from),
        Just(Value::Null),
    ]
}

/// Generate a random Vec<Value> with 0–10 entries.
fn arb_vec_value() -> impl Strategy<Value = Vec<Value>> {
    proptest::collection::vec(arb_json_value(), 0..=10)
}

/// Generate a random HashMap<String, Value> with 0–8 entries.
fn arb_map_value() -> impl Strategy<Value = HashMap<String, Value>> {
    proptest::collection::hash_map("[a-z]{1,6}".prop_map(|s| s), arb_json_value(), 0..=8)
}

/// Generate a random string (0–50 characters from a printable ASCII subset).
fn arb_string() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 !@#$%^&*()_+=\\-]{0,50}".prop_map(|s| s)
}

// ── Property 5.1: Vec<Value> round-trip ───────────────────────────────
//
// *For any* two Vec<Value> instances S1 and S2,
// Diff::apply(&S1, &Diff::diff(&S1, &S2)) == S2.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: runtime-reliability-sprint, Property 5: Delta Round-Trip (Vec<Value>)**
    /// *For any* two Vec<Value> states S1 and S2, applying the diff of S1→S2 to S1
    /// SHALL produce S2.
    /// **Validates: Requirements 16.2, 16.3**
    #[test]
    fn prop_vec_value_diff_round_trip(
        s1 in arb_vec_value(),
        s2 in arb_vec_value()
    ) {
        let delta = <Vec<Value> as Diff>::diff(&s1, &s2);
        let reconstructed = <Vec<Value> as Diff>::apply(&s1, &delta);

        prop_assert_eq!(
            &reconstructed, &s2,
            "Diff::apply(s1, Diff::diff(s1, s2)) must equal s2 for Vec<Value>"
        );
    }
}

// ── Property 5.2: HashMap<String, Value> round-trip ───────────────────
//
// *For any* two HashMap<String, Value> instances S1 and S2,
// Diff::apply(&S1, &Diff::diff(&S1, &S2)) == S2.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: runtime-reliability-sprint, Property 5: Delta Round-Trip (HashMap<String, Value>)**
    /// *For any* two HashMap<String, Value> states S1 and S2, applying the diff of S1→S2
    /// to S1 SHALL produce S2.
    /// **Validates: Requirements 16.2, 16.3**
    #[test]
    fn prop_hashmap_value_diff_round_trip(
        s1 in arb_map_value(),
        s2 in arb_map_value()
    ) {
        let delta = <HashMap<String, Value> as Diff>::diff(&s1, &s2);
        let reconstructed = <HashMap<String, Value> as Diff>::apply(&s1, &delta);

        prop_assert_eq!(
            &reconstructed, &s2,
            "Diff::apply(s1, Diff::diff(s1, s2)) must equal s2 for HashMap<String, Value>"
        );
    }
}

// ── Property 5.3: String round-trip ──────────────────────────────────
//
// *For any* two String instances S1 and S2,
// Diff::apply(&S1, &Diff::diff(&S1, &S2)) == S2.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: runtime-reliability-sprint, Property 5: Delta Round-Trip (String)**
    /// *For any* two String states S1 and S2, applying the diff of S1→S2 to S1
    /// SHALL produce S2.
    /// **Validates: Requirements 16.2, 16.3**
    #[test]
    fn prop_string_diff_round_trip(
        s1 in arb_string(),
        s2 in arb_string()
    ) {
        let delta = <String as Diff>::diff(&s1, &s2);
        let reconstructed = <String as Diff>::apply(&s1, &delta);

        prop_assert_eq!(
            &reconstructed, &s2,
            "Diff::apply(s1, Diff::diff(s1, s2)) must equal s2 for String"
        );
    }
}
