//! Shared [`VectorStore`] contract suite run against the SurrealDB backend.
//!
//! Uses the in-memory embedded engine, so no external SurrealDB server is
//! required.

#![cfg(feature = "surrealdb")]

mod common;

use adk_rag::surrealdb::SurrealVectorStore;
use common::vector_store_contract::{
    ContractOptions, arb_normalized_embedding, arb_unique_chunks, assert_vector_store_contract,
    check_search_invariants,
};
use proptest::prelude::*;

#[tokio::test]
async fn test_surrealdb_vector_store_contract() {
    let store = SurrealVectorStore::in_memory().await.expect("embedded surrealdb starts");
    assert_vector_store_contract(&store, ContractOptions::default()).await;
}

/// **VectorStore contract, search invariants (SurrealDB embedded)**
/// *For any* set of uniquely-identified chunks and any non-zero query, `search`
/// SHALL return at most `top_k` distinct stored IDs ordered by descending
/// score, and every inserted chunk SHALL be retrievable.
mod prop_surrealdb_search_invariants {
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
                let store =
                    SurrealVectorStore::in_memory().await.expect("embedded surrealdb starts");
                check_search_invariants(&store, "contract", &chunks, &query, top_k).await
            })?;
        }
    }
}
