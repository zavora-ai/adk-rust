//! Shared [`VectorStore`] contract suite run against the Qdrant backend.
//!
//! Needs a Qdrant server whose gRPC port is at `QDRANT_URL` (default
//! `http://localhost:6334`). Every collection name gets a per-run prefix, and the
//! suite deletes its collections afterwards, so a shared server is enough:
//!
//! ```bash
//! docker run -d -p 6333:6333 -p 6334:6334 qdrant/qdrant:v1.19.2
//! QDRANT_URL=http://localhost:6334 \
//!   cargo nextest run -p adk-rag --features qdrant --run-ignored only \
//!   --test vector_store_contract_qdrant
//! ```

#![cfg(feature = "qdrant")]

mod common;

use std::collections::HashMap;

use adk_rag::document::{Chunk, SearchResult};
use adk_rag::error::Result;
use adk_rag::qdrant::QdrantVectorStore;
use adk_rag::vectorstore::VectorStore;
use async_trait::async_trait;
use common::vector_store_contract::{ContractOptions, assert_vector_store_contract};
use qdrant_client::Qdrant;

fn qdrant_url() -> String {
    std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://localhost:6334".to_string())
}

/// A per-run collection prefix, so runs against one server stay apart.
fn run_prefix(test: &str) -> String {
    format!(
        "adk_rag_{test}_{}_{}_",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after the epoch")
            .as_nanos()
    )
}

/// Deletes every collection whose name starts with `prefix`.
async fn delete_collections(prefix: &str) {
    let client = Qdrant::from_url(&qdrant_url()).build().expect("qdrant client");
    let collections = client.list_collections().await.expect("list collections");
    for collection in collections.collections {
        if collection.name.starts_with(prefix) {
            client.delete_collection(collection.name).await.expect("delete collection");
        }
    }
}

/// Runs the contract against `QdrantVectorStore` with every collection name prefixed.
struct Prefixed {
    store: QdrantVectorStore,
    prefix: String,
}

impl Prefixed {
    fn name(&self, collection: &str) -> String {
        format!("{}{collection}", self.prefix)
    }
}

#[async_trait]
impl VectorStore for Prefixed {
    async fn create_collection(&self, name: &str, dimensions: usize) -> Result<()> {
        self.store.create_collection(&self.name(name), dimensions).await
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        self.store.delete_collection(&self.name(name)).await
    }

    async fn upsert(&self, collection: &str, chunks: &[Chunk]) -> Result<()> {
        self.store.upsert(&self.name(collection), chunks).await
    }

    async fn delete(&self, collection: &str, ids: &[&str]) -> Result<()> {
        self.store.delete(&self.name(collection), ids).await
    }

    async fn search(
        &self,
        collection: &str,
        embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<SearchResult>> {
        self.store.search(&self.name(collection), embedding, top_k).await
    }
}

#[tokio::test]
#[ignore = "requires a Qdrant server at QDRANT_URL"]
async fn test_qdrant_vector_store_contract() {
    let prefix = run_prefix("contract");
    let store = Prefixed { store: QdrantVectorStore::new(&qdrant_url()).expect("connect"), prefix };

    assert_vector_store_contract(&store, ContractOptions::default()).await;

    delete_collections(&store.prefix).await;
}

#[tokio::test]
#[ignore = "requires a Qdrant server at QDRANT_URL"]
async fn chunker_and_uuid_shaped_chunk_ids_round_trip() {
    let prefix = run_prefix("ids");
    let collection = format!("{prefix}docs");
    let store = QdrantVectorStore::new(&qdrant_url()).expect("connect");
    store.create_collection(&collection, 2).await.expect("create collection");

    let chunk = |id: &str, embedding: Vec<f32>| Chunk {
        id: id.to_string(),
        text: format!("text of {id}"),
        embedding,
        metadata: HashMap::from([("source".to_string(), "qdrant".to_string())]),
        document_id: "doc".to_string(),
    };
    // `doc_0` is the built-in chunkers' ID shape, which Qdrant rejects as a point ID.
    let ids = ["doc_0", "doc_1", "550e8400-e29b-41d4-a716-446655440000", "42"];
    let chunks: Vec<Chunk> =
        ids.iter().enumerate().map(|(i, id)| chunk(id, vec![1.0, i as f32])).collect();
    store.upsert(&collection, &chunks).await.expect("upsert");
    // Re-upserting a chunk ID replaces its point.
    store.upsert(&collection, &chunks[1..2]).await.expect("upsert again");

    let mut found: Vec<Chunk> = store
        .search(&collection, &[1.0, 0.0], 10)
        .await
        .expect("search")
        .into_iter()
        .map(|result| result.chunk)
        .collect();
    found.sort_by(|a, b| a.id.cmp(&b.id));
    let mut expected: Vec<Chunk> =
        chunks.iter().map(|chunk| Chunk { embedding: vec![], ..chunk.clone() }).collect();
    expected.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(found, expected);

    store.delete(&collection, &["doc_0", "42"]).await.expect("delete");
    let mut remaining: Vec<String> = store
        .search(&collection, &[1.0, 0.0], 10)
        .await
        .expect("search")
        .into_iter()
        .map(|result| result.chunk.id)
        .collect();
    remaining.sort();
    assert_eq!(remaining, ["550e8400-e29b-41d4-a716-446655440000", "doc_1"]);

    delete_collections(&prefix).await;
}
