//! Integration tests for the SurrealDB vector store backend.
//!
//! These tests use the in-memory embedded engine, so no external
//! SurrealDB server is required.

#![cfg(feature = "surrealdb")]

use adk_rag::document::Chunk;
use adk_rag::surrealdb::SurrealVectorStore;
use adk_rag::{CollectionSpec, Scope, VectorQuery, VectorStore};

/// Helper: create a chunk with a given ID, text, and embedding.
fn make_chunk(id: &str, text: &str, embedding: Vec<f32>, doc_id: &str) -> Chunk {
    Chunk::new(id, doc_id, text).with_embedding(embedding).with_metadata("source", "test")
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
    store.create_collection("test_col", &CollectionSpec::new(3)).await.unwrap();
    // Creating again should be a no-op
    store.create_collection("test_col", &CollectionSpec::new(3)).await.unwrap();
    store.delete_collection("test_col").await.unwrap();
}

#[tokio::test]
async fn test_upsert_and_search() {
    let store = SurrealVectorStore::in_memory().await.unwrap();
    store.create_collection("docs", &CollectionSpec::new(3)).await.unwrap();

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

    store.upsert("docs", &Scope::single_tenant(), &chunks).await.unwrap();

    // Search with a query close to emb1 (rust)
    let results =
        store.search("docs", &Scope::single_tenant(), &VectorQuery::new(&emb1, 2)).await.unwrap();
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
    store.create_collection("docs", &CollectionSpec::new(3)).await.unwrap();

    let mut emb = vec![1.0, 0.0, 0.0];
    normalize(&mut emb);

    let chunk_v1 = make_chunk("c1", "version one", emb.clone(), "doc1");
    store.upsert("docs", &Scope::single_tenant(), &[chunk_v1]).await.unwrap();

    let chunk_v2 = make_chunk("c1", "version two", emb.clone(), "doc1");
    store.upsert("docs", &Scope::single_tenant(), &[chunk_v2]).await.unwrap();

    let results =
        store.search("docs", &Scope::single_tenant(), &VectorQuery::new(&emb, 10)).await.unwrap();
    // Should only have one record (upsert replaced it)
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.text, "version two");
}

#[tokio::test]
async fn test_delete_chunks() {
    let store = SurrealVectorStore::in_memory().await.unwrap();
    store.create_collection("docs", &CollectionSpec::new(3)).await.unwrap();

    let mut emb = vec![1.0, 0.0, 0.0];
    normalize(&mut emb);

    let chunks = vec![
        make_chunk("c1", "first", emb.clone(), "doc1"),
        make_chunk("c2", "second", emb.clone(), "doc1"),
    ];
    store.upsert("docs", &Scope::single_tenant(), &chunks).await.unwrap();

    store.delete("docs", &Scope::single_tenant(), &["c1"]).await.unwrap();

    let results =
        store.search("docs", &Scope::single_tenant(), &VectorQuery::new(&emb, 10)).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.text, "second");
}

#[tokio::test]
async fn test_search_empty_collection() {
    let store = SurrealVectorStore::in_memory().await.unwrap();
    store.create_collection("empty", &CollectionSpec::new(3)).await.unwrap();

    let results = store
        .search("empty", &Scope::single_tenant(), &VectorQuery::new(&[1.0, 0.0, 0.0], 5))
        .await
        .unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn test_metadata_preserved() {
    let store = SurrealVectorStore::in_memory().await.unwrap();
    store.create_collection("meta", &CollectionSpec::new(3)).await.unwrap();

    let mut emb = vec![1.0, 0.0, 0.0];
    normalize(&mut emb);

    let mut chunk = make_chunk("c1", "with metadata", emb.clone(), "doc1");
    chunk.metadata.insert("custom_key".to_string(), "custom_value".to_string());

    store.upsert("meta", &Scope::single_tenant(), &[chunk]).await.unwrap();

    let results =
        store.search("meta", &Scope::single_tenant(), &VectorQuery::new(&emb, 1)).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.metadata.get("custom_key").unwrap(), "custom_value");
    assert_eq!(results[0].chunk.metadata.get("source").unwrap(), "test");
}

#[tokio::test]
async fn collection_names_that_would_need_rewriting_are_refused() {
    let store = SurrealVectorStore::in_memory().await.unwrap();
    let chunk = make_chunk("c1", "text", vec![1.0, 0.0, 0.0], "doc1");
    for name in ["docs-v1", "docs v1", "docs.v1", "naïve", ""] {
        let expected = format!(
            "Vector store error (surrealdb): invalid collection name `{name}`: use one or more \
             ASCII letters, digits, or underscores"
        );
        let errors = [
            store.create_collection(name, &CollectionSpec::new(3)).await.err(),
            store.upsert(name, &Scope::single_tenant(), std::slice::from_ref(&chunk)).await.err(),
            store
                .search(name, &Scope::single_tenant(), &VectorQuery::new(&[1.0, 0.0, 0.0], 1))
                .await
                .err(),
            store.delete(name, &Scope::single_tenant(), &["c1"]).await.err(),
            store.delete_collection(name).await.err(),
        ];
        assert_eq!(
            errors.map(|error| error.map(|error| error.to_string())),
            [(); 5].map(|()| Some(expected.clone())),
            "collection name {name:?}"
        );
    }
}

#[tokio::test]
async fn keywords_leading_digits_and_case_are_distinct_valid_names() {
    let store = SurrealVectorStore::in_memory().await.unwrap();
    let names = ["select", "table", "9lives", "Docs", "docs"];
    for (i, name) in names.iter().enumerate() {
        store.create_collection(name, &CollectionSpec::new(3)).await.unwrap();
        let chunk = make_chunk(&format!("row_{i}"), name, vec![1.0, 0.0, 0.0], "doc1");
        store.upsert(name, &Scope::single_tenant(), &[chunk]).await.unwrap();
    }
    for (i, name) in names.iter().enumerate() {
        let results = store
            .search(name, &Scope::single_tenant(), &VectorQuery::new(&[1.0, 0.0, 0.0], 10))
            .await
            .unwrap();
        let ids: Vec<&str> = results.iter().map(|result| result.chunk.id.as_str()).collect();
        assert_eq!(ids, [format!("row_{i}")], "collection {name:?}");
        store.delete_collection(name).await.unwrap();
    }
}
