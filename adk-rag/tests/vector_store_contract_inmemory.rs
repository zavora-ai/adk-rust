//! Shared [`VectorStore`] contract suite run against the in-memory backend.

mod common;

use adk_rag::inmemory::InMemoryVectorStore;
use common::vector_store_contract::{
    ContractOptions, arb_normalized_embedding, arb_unique_chunks, assert_vector_store_contract,
    check_search_invariants,
};
use proptest::prelude::*;

#[tokio::test]
async fn test_inmemory_vector_store_contract() {
    let store = InMemoryVectorStore::new();
    assert_vector_store_contract(&store, ContractOptions::default()).await;
}

/// **VectorStore contract, search invariants (in-memory)**
/// *For any* set of uniquely-identified chunks and any non-zero query, `search`
/// SHALL return at most `top_k` distinct stored IDs ordered by descending
/// score, and every inserted chunk SHALL be retrievable.
mod prop_inmemory_search_invariants {
    use super::*;

    const DIM: usize = 8;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn search_invariants_hold(
            chunks in arb_unique_chunks(DIM, 20),
            query in arb_normalized_embedding(DIM),
            top_k in 1usize..25,
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryVectorStore::new();
                check_search_invariants(&store, "contract", &chunks, &query, top_k).await
            })?;
        }
    }
}

/// **VectorStore contract, filters (differential)**
/// *For any* generated filter, search and count SHALL select exactly the chunks
/// `Filter::matches` selects.
mod prop_inmemory_filters_match_the_reference {
    use super::*;
    use common::vector_store_contract::{arb_filter, check_generated_filters};

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn generated_filters_match_the_reference(
            filters in proptest::collection::vec(arb_filter(), 1..8),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryVectorStore::new();
                check_generated_filters(&store, &filters, false).await
            })?;
        }
    }
}
