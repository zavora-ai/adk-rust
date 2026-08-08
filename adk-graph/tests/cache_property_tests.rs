//! Property tests for node cache determinism.
//!
//! **Feature: runtime-reliability-sprint, Property 4: Cache Determinism**
//! *For any* node with caching enabled, executing the node with identical input state
//! SHALL produce identical output regardless of whether the result came from cache or
//! fresh execution.
//! **Validates: Requirements 9.3, 9.4**

#![cfg(feature = "node-cache")]

use std::collections::HashMap;
use std::time::Duration;

use adk_graph::cache::{CacheBackend, NodeCache, NodeCachePolicy, compute_cache_key};
use proptest::prelude::*;
use serde_json::Value;

// ── Generators ────────────────────────────────────────────────────────

/// Generate a random node name (1–20 alphanumeric characters).
fn arb_node_name() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,19}".prop_map(|s| s)
}

/// Generate a random JSON value suitable for state entries.
fn arb_json_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<f64>().prop_filter("must be finite", |f| f.is_finite()).prop_map(Value::from),
        "[a-zA-Z0-9 ]{0,30}".prop_map(Value::from),
        Just(Value::Null),
    ]
}

/// Generate a random state map with 0–5 entries.
fn arb_state() -> impl Strategy<Value = HashMap<String, Value>> {
    proptest::collection::hash_map("[a-z]{1,8}".prop_map(|s| s), arb_json_value(), 0..=5)
}

// ── Property 4.1: Cache key determinism ───────────────────────────────
//
// *For any* node name and input state, computing the cache key twice with
// the same inputs SHALL produce the same key.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_cache_key_same_inputs_produce_same_key(
        node_name in arb_node_name(),
        state in arb_state()
    ) {
        let key1 = compute_cache_key(&node_name, &state);
        let key2 = compute_cache_key(&node_name, &state);

        prop_assert_eq!(&key1, &key2, "Same inputs must produce identical cache keys");
        prop_assert_eq!(key1.len(), 64, "Cache key must be a 64-char hex blake3 digest");
    }
}

// ── Property 4.2: Cache get returns stored value ──────────────────────
//
// *For any* cache key and value, storing a value and then retrieving it
// SHALL return the exact same value.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_cache_get_returns_stored_value(
        node_name in arb_node_name(),
        state in arb_state(),
        result_value in arb_json_value()
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        rt.block_on(async {
            let policy = NodeCachePolicy {
                backend: CacheBackend::InMemory { max_entries: 128 },
                ttl: Some(Duration::from_secs(300)),
            };
            let cache = NodeCache::from_policy(&policy);

            let key = compute_cache_key(&node_name, &state);

            // Store the value
            cache.set(&key, result_value.clone(), policy.ttl).await;

            // Retrieve it
            let cached = cache.get(&key).await;

            prop_assert_eq!(
                cached.as_ref(),
                Some(&result_value),
                "Retrieved value must equal stored value"
            );

            Ok(())
        })?;
    }
}

// ── Property 4.3: Different inputs produce different cache keys ───────
//
// *For any* two distinct (node_name, state) pairs, the cache keys SHALL
// differ (with overwhelming probability due to blake3 collision resistance).

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_different_inputs_produce_different_keys(
        node_name_a in arb_node_name(),
        node_name_b in arb_node_name(),
        state_a in arb_state(),
        state_b in arb_state()
    ) {
        // Only test when inputs actually differ
        let inputs_differ = node_name_a != node_name_b || state_a != state_b;
        prop_assume!(inputs_differ);

        let key_a = compute_cache_key(&node_name_a, &state_a);
        let key_b = compute_cache_key(&node_name_b, &state_b);

        prop_assert_ne!(
            &key_a, &key_b,
            "Different inputs must produce different cache keys"
        );
    }
}

// ── Property 4.4: Cache key is independent of HashMap insertion order ─
//
// *For any* state map, inserting keys in different orders SHALL produce
// the same cache key (since we sort keys before hashing).

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_cache_key_independent_of_insertion_order(
        node_name in arb_node_name(),
        state in arb_state()
    ) {
        // Collect entries and re-insert in reverse order to create a different
        // internal HashMap layout.
        let entries: Vec<(String, Value)> = state.iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        // Build state in reverse insertion order
        let mut state_reverse: HashMap<String, Value> = HashMap::new();
        for (k, v) in entries.iter().rev() {
            state_reverse.insert(k.clone(), v.clone());
        }

        let key_original = compute_cache_key(&node_name, &state);
        let key_reverse = compute_cache_key(&node_name, &state_reverse);

        prop_assert_eq!(
            &key_original, &key_reverse,
            "Cache key must be independent of HashMap insertion order"
        );
    }
}

// ── Property 4.5: Second call returns cached result (full round-trip) ─
//
// *For any* node with caching enabled, executing the node twice with
// identical state SHALL return the same cached result on the second call.

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_second_call_returns_cached_result(
        node_name in arb_node_name(),
        state in arb_state(),
        execution_result in arb_json_value()
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        rt.block_on(async {
            let policy = NodeCachePolicy {
                backend: CacheBackend::InMemory { max_entries: 256 },
                ttl: Some(Duration::from_secs(600)),
            };
            let cache = NodeCache::from_policy(&policy);

            // Simulate first execution: compute key, store result
            let key = compute_cache_key(&node_name, &state);
            cache.set(&key, execution_result.clone(), policy.ttl).await;

            // Simulate second execution: compute key again, check cache
            let key_again = compute_cache_key(&node_name, &state);
            let cached = cache.get(&key_again).await;

            prop_assert_eq!(
                cached.as_ref(),
                Some(&execution_result),
                "Second call with identical state must return cached result"
            );

            Ok(())
        })?;
    }
}
