//! Runtime integration tests for the LanceDB vector store backend.
//!
//! LanceDB runs embedded against a local directory, so these tests exercise the
//! real storage engine with no external service and no API key. Each test gets
//! its own [`TempDir`] so they stay independent under parallel execution.

#![cfg(feature = "lancedb")]

use std::collections::HashMap;

use adk_rag::document::Chunk;
use adk_rag::lancedb::LanceDBVectorStore;
use adk_rag::vectorstore::VectorStore;
use tempfile::TempDir;

const DIM: usize = 4;

/// Build a store rooted in a fresh temporary directory.
///
/// The `TempDir` is returned alongside the store so the caller keeps it alive
/// for the duration of the test.
async fn store() -> (TempDir, LanceDBVectorStore) {
    let dir = TempDir::new().expect("create temp dir");
    let store = LanceDBVectorStore::new(dir.path().to_str().expect("utf-8 temp path"))
        .await
        .expect("connect to embedded lancedb");
    (dir, store)
}

fn chunk(id: &str, text: &str, embedding: Vec<f32>, metadata: HashMap<String, String>) -> Chunk {
    Chunk {
        id: id.to_string(),
        text: text.to_string(),
        embedding,
        metadata,
        document_id: "doc_1".to_string(),
    }
}

fn plain(id: &str, text: &str, embedding: Vec<f32>) -> Chunk {
    chunk(id, text, embedding, HashMap::new())
}

/// `search` never reads the vector column back, so every returned chunk has an
/// empty embedding. This mirrors a stored chunk into the shape `search` yields.
fn as_returned(c: &Chunk) -> Chunk {
    Chunk { embedding: vec![], ..c.clone() }
}

fn ids(results: &[adk_rag::document::SearchResult]) -> Vec<String> {
    results.iter().map(|r| r.chunk.id.clone()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn test_lancedb_full_round_trip() {
    let (_dir, store) = store().await;
    store.create_collection("docs", DIM).await.unwrap();

    let alpha = plain("a", "alpha text", vec![1.0, 0.0, 0.0, 0.0]);
    let beta = plain("b", "beta text", vec![0.0, 1.0, 0.0, 0.0]);
    let gamma = plain("c", "gamma text", vec![0.0, 0.0, 1.0, 0.0]);
    let chunks = vec![alpha.clone(), beta.clone(), gamma.clone()];

    store.upsert("docs", &chunks).await.unwrap();

    let results = store.search("docs", &[0.0, 0.9, 0.0, 0.0], 3).await.unwrap();
    assert_eq!(results.len(), 3, "expected all three stored rows back");
    assert_eq!(results[0].chunk, as_returned(&beta), "nearest row should be the beta chunk");
    assert_eq!(ids(&results)[0], "b");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_lancedb_metadata_round_trip() {
    let (_dir, store) = store().await;
    store.create_collection("docs", DIM).await.unwrap();

    let mut metadata = HashMap::new();
    metadata.insert("source".to_string(), "handbook.pdf".to_string());
    metadata.insert("page".to_string(), "12".to_string());
    metadata.insert("quoted".to_string(), r#"he said "hello" \ then left"#.to_string());
    metadata.insert("unicode".to_string(), "naïve café — 日本語 🚀".to_string());
    metadata.insert("newlines".to_string(), "line1\nline2\ttabbed".to_string());

    let stored = chunk("meta-1", "metadata carrier", vec![1.0, 0.0, 0.0, 0.0], metadata.clone());
    store.upsert("docs", std::slice::from_ref(&stored)).await.unwrap();

    let results = store.search("docs", &[1.0, 0.0, 0.0, 0.0], 1).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.metadata, metadata);
    assert_eq!(results[0].chunk, as_returned(&stored));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_lancedb_similarity_ordering() {
    let (_dir, store) = store().await;
    store.create_collection("docs", DIM).await.unwrap();

    // Query is [0,0,0,0]; each chunk sits at a strictly increasing L2 distance.
    let chunks = vec![
        plain("near", "near", vec![1.0, 0.0, 0.0, 0.0]),
        plain("mid", "mid", vec![3.0, 0.0, 0.0, 0.0]),
        plain("far", "far", vec![9.0, 0.0, 0.0, 0.0]),
        plain("farthest", "farthest", vec![27.0, 0.0, 0.0, 0.0]),
    ];
    store.upsert("docs", &chunks).await.unwrap();

    let results = store.search("docs", &[0.0, 0.0, 0.0, 0.0], 4).await.unwrap();
    assert_eq!(ids(&results), vec!["near", "mid", "far", "farthest"]);

    // score = 1.0 - distance, so scores must be monotonically non-increasing.
    for pair in results.windows(2) {
        assert!(
            pair[0].score >= pair[1].score,
            "scores not non-increasing: {} then {}",
            pair[0].score,
            pair[1].score,
        );
    }
    // The scores must actually differ, otherwise the ordering check is vacuous.
    assert!(
        results[0].score > results[results.len() - 1].score,
        "expected distinct scores across distinct distances, got {:?}",
        results.iter().map(|r| r.score).collect::<Vec<_>>(),
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_lancedb_top_k_bound() {
    let (_dir, store) = store().await;
    store.create_collection("docs", DIM).await.unwrap();

    let chunks: Vec<Chunk> =
        (0..7).map(|i| plain(&format!("id-{i}"), "row", vec![i as f32, 0.0, 0.0, 0.0])).collect();
    store.upsert("docs", &chunks).await.unwrap();

    let results = store.search("docs", &[0.0, 0.0, 0.0, 0.0], 3).await.unwrap();
    assert_eq!(results.len(), 3, "top_k must bound the result count");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_lancedb_delete_by_id() {
    let (_dir, store) = store().await;
    store.create_collection("docs", DIM).await.unwrap();

    let chunks = vec![
        plain("keep-1", "keep one", vec![1.0, 0.0, 0.0, 0.0]),
        plain("drop-1", "drop one", vec![0.0, 1.0, 0.0, 0.0]),
        plain("keep-2", "keep two", vec![0.0, 0.0, 1.0, 0.0]),
        plain("drop-2", "drop two", vec![0.0, 0.0, 0.0, 1.0]),
    ];
    store.upsert("docs", &chunks).await.unwrap();
    assert_eq!(store.search("docs", &[0.0, 0.0, 0.0, 0.0], 10).await.unwrap().len(), 4);

    store.delete("docs", &["drop-1", "drop-2"]).await.unwrap();

    let mut remaining = ids(&store.search("docs", &[0.0, 0.0, 0.0, 0.0], 10).await.unwrap());
    remaining.sort();
    assert_eq!(remaining, vec!["keep-1".to_string(), "keep-2".to_string()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_lancedb_create_collection_idempotency() {
    let (_dir, store) = store().await;
    store.create_collection("docs", DIM).await.unwrap();

    let stored = plain("only", "survivor", vec![1.0, 0.0, 0.0, 0.0]);
    store.upsert("docs", std::slice::from_ref(&stored)).await.unwrap();

    // Second call must succeed and must not drop the existing rows.
    store.create_collection("docs", DIM).await.unwrap();

    let results = store.search("docs", &[1.0, 0.0, 0.0, 0.0], 5).await.unwrap();
    assert_eq!(results.len(), 1, "existing rows must survive a repeated create_collection");
    assert_eq!(results[0].chunk, as_returned(&stored));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_lancedb_delete_collection() {
    let (_dir, store) = store().await;
    store.create_collection("docs", DIM).await.unwrap();
    store.upsert("docs", &[plain("a", "alpha", vec![1.0, 0.0, 0.0, 0.0])]).await.unwrap();
    assert_eq!(store.search("docs", &[1.0, 0.0, 0.0, 0.0], 1).await.unwrap().len(), 1);

    store.delete_collection("docs").await.unwrap();

    // The table is gone, so opening it for a search fails.
    let err = store.search("docs", &[1.0, 0.0, 0.0, 0.0], 1).await.unwrap_err();
    assert!(
        matches!(err, adk_rag::error::RagError::VectorStoreError { .. }),
        "expected a vector store error after delete_collection, got {err:?}"
    );
    // Upsert into the dropped collection also fails.
    assert!(
        store.upsert("docs", &[plain("b", "beta", vec![0.0, 1.0, 0.0, 0.0])]).await.is_err(),
        "upsert into a dropped collection must fail"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_lancedb_empty_input_noops() {
    let (_dir, store) = store().await;
    store.create_collection("docs", DIM).await.unwrap();

    let chunks = vec![
        plain("a", "alpha", vec![1.0, 0.0, 0.0, 0.0]),
        plain("b", "beta", vec![0.0, 1.0, 0.0, 0.0]),
    ];
    store.upsert("docs", &chunks).await.unwrap();
    let before = ids(&store.search("docs", &[0.0, 0.0, 0.0, 0.0], 10).await.unwrap());
    assert_eq!(before.len(), 2);

    store.upsert("docs", &[]).await.unwrap();
    store.delete("docs", &[]).await.unwrap();

    let after = ids(&store.search("docs", &[0.0, 0.0, 0.0, 0.0], 10).await.unwrap());
    assert_eq!(after, before, "empty-input calls must not change stored rows");
}
