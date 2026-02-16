//! Integration tests for the SurrealDB vector store backend.
//!
//! These tests use the in-memory embedded engine, so no external
//! SurrealDB server is required.

#![cfg(feature = "surrealdb")]

use std::collections::HashMap;

use adk_rag::VectorStore;
use adk_rag::document::Chunk;
use adk_rag::surrealdb::SurrealVectorStore;

/// Helper: create a chunk with a given ID, text, and embedding.
fn make_chunk(id: &str, text: &str, embedding: Vec<f32>, doc_id: &str) -> Chunk {
    Chunk {
        id: id.to_string(),
        text: text.to_string(),
        embedding,
        metadata: HashMap::from([("source".to_string(), "test".to_string())]),
        document_id: doc_id.to_string(),
    }
}

/// L2-normalize a vector in place.
fn normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

#[tokio::test]
async fn test_create_and_delete_collection() {
    let store = SurrealVectorStore::in_memory().await.unwrap();
    store.create_collection("test_col", 3).await.unwrap();
    // Creating again should be a no-op
    store.create_collection("test_col", 3).await.unwrap();
    store.delete_collection("test_col").await.unwrap();
}

#[tokio::test]
async fn test_upsert_and_search() {
    let store = SurrealVectorStore::in_memory().await.unwrap();
    store.create_collection("docs", 3).await.unwrap();

    let mut emb1 = vec![1.0, 0.0, 0.0];
    let mut emb2 = vec![0.0, 1.0, 0.0];
    let mut emb3 = vec![0.7, 0.7, 0.0];
    normalize(&mut emb1);
    normalize(&mut emb2);
    normalize(&mut emb3);

    let chunks = vec![
        make_chunk("c1", "about rust", emb1.clone(), "doc1"),
        make_chunk("c2", "about python", emb2.clone(), "doc1"),
        make_chunk("c3", "about both", emb3.clone(), "doc2"),
    ];

    store.upsert("docs", &chunks).await.unwrap();

    // Search with a query close to emb1 (rust)
    let results = store.search("docs", &emb1, 2).await.unwrap();
    assert!(!results.is_empty());
    assert!(results.len() <= 2);
    // The closest result should be "about rust"
    assert_eq!(results[0].chunk.text, "about rust");
    // Score should be close to 1.0 for exact match
    assert!(results[0].score > 0.9, "expected high score, got {}", results[0].score);
}

#[tokio::test]
async fn test_upsert_overwrites() {
    let store = SurrealVectorStore::in_memory().await.unwrap();
    store.create_collection("docs", 3).await.unwrap();

    let mut emb = vec![1.0, 0.0, 0.0];
    normalize(&mut emb);

    let chunk_v1 = make_chunk("c1", "version one", emb.clone(), "doc1");
    store.upsert("docs", &[chunk_v1]).await.unwrap();

    let chunk_v2 = make_chunk("c1", "version two", emb.clone(), "doc1");
    store.upsert("docs", &[chunk_v2]).await.unwrap();

    let results = store.search("docs", &emb, 10).await.unwrap();
    // Should only have one record (upsert replaced it)
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.text, "version two");
}

#[tokio::test]
async fn test_delete_chunks() {
    let store = SurrealVectorStore::in_memory().await.unwrap();
    store.create_collection("docs", 3).await.unwrap();

    let mut emb = vec![1.0, 0.0, 0.0];
    normalize(&mut emb);

    let chunks = vec![
        make_chunk("c1", "first", emb.clone(), "doc1"),
        make_chunk("c2", "second", emb.clone(), "doc1"),
    ];
    store.upsert("docs", &chunks).await.unwrap();

    store.delete("docs", &["c1"]).await.unwrap();

    let results = store.search("docs", &emb, 10).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.text, "second");
}

#[tokio::test]
async fn test_search_empty_collection() {
    let store = SurrealVectorStore::in_memory().await.unwrap();
    store.create_collection("empty", 3).await.unwrap();

    let results = store.search("empty", &[1.0, 0.0, 0.0], 5).await.unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn test_metadata_preserved() {
    let store = SurrealVectorStore::in_memory().await.unwrap();
    store.create_collection("meta", 3).await.unwrap();

    let mut emb = vec![1.0, 0.0, 0.0];
    normalize(&mut emb);

    let mut chunk = make_chunk("c1", "with metadata", emb.clone(), "doc1");
    chunk.metadata.insert("custom_key".to_string(), "custom_value".to_string());

    store.upsert("meta", &[chunk]).await.unwrap();

    let results = store.search("meta", &emb, 1).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.metadata.get("custom_key").unwrap(), "custom_value");
    assert_eq!(results[0].chunk.metadata.get("source").unwrap(), "test");
}
