//! Shared [`VectorStore`] contract suite run against the LanceDB backend.
//!
//! LanceDB runs embedded against a local directory, so the suite exercises the
//! real storage engine with no external service. The test gets its own
//! [`TempDir`] so it stays independent under parallel execution.

#![cfg(feature = "lancedb")]

mod common;

use adk_rag::lancedb::LanceDBVectorStore;
use common::vector_store_contract::{ContractOptions, assert_vector_store_contract};
use proptest::prelude::*;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread")]
async fn test_lancedb_vector_store_contract() {
    let dir = TempDir::new().expect("create temp dir");
    let store = LanceDBVectorStore::new(dir.path().to_str().expect("utf-8 temp path"))
        .await
        .expect("connect to embedded lancedb");

    assert_vector_store_contract(&store, ContractOptions::default()).await;
}

/// **VectorStore contract, filters (differential)**
/// *For any* generated filter, search and count SHALL select exactly the chunks
/// `Filter::matches` selects.
mod prop_lancedb_filters_match_the_reference {
    use super::*;
    use common::vector_store_contract::{arb_filter, check_generated_filters};

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(30))]

        #[test]
        fn generated_filters_match_the_reference(
            filters in proptest::collection::vec(arb_filter(), 1..8),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let dir = TempDir::new().expect("create temp dir");
                let store = LanceDBVectorStore::new(dir.path().to_str().expect("utf-8 temp path"))
                    .await
                    .expect("connect to embedded lancedb");
                check_generated_filters(&store, &filters, false).await
            })?;
        }
    }
}
