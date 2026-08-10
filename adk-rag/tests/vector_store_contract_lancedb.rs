//! Shared [`VectorStore`] contract suite run against the LanceDB backend.
//!
//! LanceDB runs embedded against a local directory, so the suite exercises the
//! real storage engine with no external service. The test gets its own
//! [`TempDir`] so it stays independent under parallel execution.

#![cfg(feature = "lancedb")]

mod common;

use adk_rag::lancedb::LanceDBVectorStore;
use common::vector_store_contract::{ContractOptions, assert_vector_store_contract};
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread")]
async fn test_lancedb_vector_store_contract() {
    let dir = TempDir::new().expect("create temp dir");
    let store = LanceDBVectorStore::new(dir.path().to_str().expect("utf-8 temp path"))
        .await
        .expect("connect to embedded lancedb");

    assert_vector_store_contract(
        &store,
        ContractOptions {
            // LanceDB's `upsert` appends via `table.add` instead of replacing
            // by ID, so a repeated ID yields duplicate rows — a behavioral
            // divergence from InMemory and SurrealDB. The replacement
            // assertion is scoped out until the backend gains true upsert
            // semantics.
            upsert_replaces_by_id: false,
        },
    )
    .await;
}
