//! Property-based tests for Agent Registry storage.
//!
//! Tests two correctness properties from the design document:
//! - Property 3: Agent Registry Insert-List Round-Trip
//! - Property 4: Agent Registry Filter Correctness

#![cfg(feature = "agent-registry")]

use std::collections::HashSet;

use proptest::prelude::*;

use adk_server::registry::store::{AgentFilter, AgentRegistryStore, InMemoryAgentRegistryStore};
use adk_server::registry::types::AgentCard;

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

/// Generate a simple alphanumeric identifier suitable for agent names.
fn arb_identifier() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_-]{0,15}"
}

/// Generate a version string like "X.Y.Z".
fn arb_version() -> impl Strategy<Value = String> {
    (0u32..20, 0u32..20, 0u32..20).prop_map(|(a, b, c)| format!("{a}.{b}.{c}"))
}

/// Generate an optional description.
fn arb_optional_string() -> impl Strategy<Value = Option<String>> {
    prop_oneof![Just(None), "[a-zA-Z0-9 ._-]{1,40}".prop_map(Some),]
}

/// Generate a small set of tags from a known pool to increase filter hit rate.
fn arb_tags() -> impl Strategy<Value = Vec<String>> {
    let tag_pool = vec!["search", "qa", "chat", "code", "data", "voice", "tool", "rag"];
    prop::collection::vec(prop::sample::select(tag_pool).prop_map(|s| s.to_string()), 0..4)
        .prop_map(|tags| {
            // Deduplicate tags
            let mut seen = HashSet::new();
            tags.into_iter().filter(|t| seen.insert(t.clone())).collect()
        })
}

/// Generate a single AgentCard with a given name (to ensure distinctness).
fn arb_agent_card_with_name(name: String) -> impl Strategy<Value = AgentCard> {
    (arb_version(), arb_optional_string(), arb_tags(), arb_optional_string()).prop_map(
        move |(version, description, tags, endpoint_url)| AgentCard {
            name: name.clone(),
            version,
            description,
            tags,
            endpoint_url,
            capabilities: vec![],
            input_modes: vec![],
            output_modes: vec![],
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: None,
        },
    )
}

/// Generate a set of N distinct AgentCards (distinct by name).
fn arb_distinct_agent_cards(max_count: usize) -> impl Strategy<Value = Vec<AgentCard>> {
    prop::collection::hash_set(arb_identifier(), 1..=max_count).prop_flat_map(|names| {
        let strategies: Vec<_> =
            names.into_iter().map(|name| arb_agent_card_with_name(name).boxed()).collect();
        strategies
    })
}

/// Generate an AgentFilter with optional name_prefix and tag from known pools.
fn arb_agent_filter() -> impl Strategy<Value = AgentFilter> {
    let prefix_pool = vec![
        None,
        Some("a".to_string()),
        Some("b".to_string()),
        Some("c".to_string()),
        Some("search".to_string()),
    ];
    let tag_pool = vec![
        None,
        Some("search".to_string()),
        Some("qa".to_string()),
        Some("chat".to_string()),
        Some("code".to_string()),
        Some("data".to_string()),
        Some("voice".to_string()),
        Some("tool".to_string()),
        Some("rag".to_string()),
    ];
    (prop::sample::select(prefix_pool), prop::sample::select(tag_pool))
        .prop_map(|(name_prefix, tag)| AgentFilter { name_prefix, tag, version_range: None })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Reference implementation of filter matching (mirrors store.rs logic).
fn card_matches_filter(card: &AgentCard, filter: &AgentFilter) -> bool {
    if let Some(prefix) = &filter.name_prefix {
        if !card.name.starts_with(prefix.as_str()) {
            return false;
        }
    }
    if let Some(tag) = &filter.tag {
        if !card.tags.contains(tag) {
            return false;
        }
    }
    true
}

/// Create a tokio runtime for running async tests inside proptest.
fn run_async<F: std::future::Future<Output = Result<(), TestCaseError>>>(
    f: F,
) -> Result<(), TestCaseError> {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
}

// ---------------------------------------------------------------------------
// Property 3: Agent Registry Insert-List Round-Trip
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(120))]

    /// **Feature: competitive-parity-v070, Property 3: Agent Registry Insert-List Round-Trip**
    ///
    /// *For any* set of N distinct AgentCards (distinct by name), inserting all N
    /// into the AgentRegistryStore and then calling `list` with no filter SHALL
    /// return exactly N cards, and each returned card SHALL be equal to the
    /// corresponding inserted card.
    ///
    /// **Validates: Requirements 3.1, 3.2**
    #[test]
    fn prop_registry_insert_list_round_trip(cards in arb_distinct_agent_cards(10)) {
        run_async(async {
            let store = InMemoryAgentRegistryStore::new();

            // Insert all cards
            for card in &cards {
                store.insert(card.clone()).await
                    .map_err(|e| TestCaseError::fail(format!("insert failed: {e}")))?;
            }

            // List with empty filter
            let listed = store.list(&AgentFilter::default()).await
                .map_err(|e| TestCaseError::fail(format!("list failed: {e}")))?;

            // Assert count matches
            prop_assert_eq!(
                listed.len(),
                cards.len(),
                "listed count should equal inserted count"
            );

            // Assert all inserted cards are present in the listed results
            let listed_names: HashSet<&str> = listed.iter().map(|c| c.name.as_str()).collect();
            for card in &cards {
                prop_assert!(
                    listed_names.contains(card.name.as_str()),
                    "inserted card '{}' should be in listed results",
                    card.name
                );

                // Find the matching card and verify equality
                let found = listed.iter().find(|c| c.name == card.name).unwrap();
                prop_assert_eq!(found, card, "listed card should equal inserted card");
            }

            Ok(())
        })?;
    }
}

// ---------------------------------------------------------------------------
// Property 4: Agent Registry Filter Correctness
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(120))]

    /// **Feature: competitive-parity-v070, Property 4: Agent Registry Filter Correctness**
    ///
    /// *For any* set of AgentCards and any AgentFilter, `list(filter)` returns
    /// exactly those cards matching the filter — no false positives and no false
    /// negatives.
    ///
    /// **Validates: Requirements 3.3**
    #[test]
    fn prop_registry_filter_correctness(
        cards in arb_distinct_agent_cards(10),
        filter in arb_agent_filter(),
    ) {
        run_async(async {
            let store = InMemoryAgentRegistryStore::new();

            // Insert all cards
            for card in &cards {
                store.insert(card.clone()).await
                    .map_err(|e| TestCaseError::fail(format!("insert failed: {e}")))?;
            }

            // List with the generated filter
            let filtered = store.list(&filter).await
                .map_err(|e| TestCaseError::fail(format!("list failed: {e}")))?;

            // Compute expected results using reference implementation
            let expected: Vec<&AgentCard> = cards
                .iter()
                .filter(|card| card_matches_filter(card, &filter))
                .collect();

            // No false negatives: every expected card is in the result
            let filtered_names: HashSet<&str> =
                filtered.iter().map(|c| c.name.as_str()).collect();
            for card in &expected {
                prop_assert!(
                    filtered_names.contains(card.name.as_str()),
                    "expected card '{}' missing from filtered results (false negative). \
                     Filter: name_prefix={:?}, tag={:?}",
                    card.name,
                    filter.name_prefix,
                    filter.tag,
                );
            }

            // No false positives: every result card is in the expected set
            let expected_names: HashSet<&str> =
                expected.iter().map(|c| c.name.as_str()).collect();
            for card in &filtered {
                prop_assert!(
                    expected_names.contains(card.name.as_str()),
                    "unexpected card '{}' in filtered results (false positive). \
                     Filter: name_prefix={:?}, tag={:?}",
                    card.name,
                    filter.name_prefix,
                    filter.tag,
                );
            }

            // Count must match exactly
            prop_assert_eq!(
                filtered.len(),
                expected.len(),
                "filtered count should match expected count"
            );

            Ok(())
        })?;
    }
}
