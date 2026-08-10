//! Shared behavioral contract for [`VectorStore`] backends.
//!
//! [`assert_vector_store_contract`] walks one store through the semantics every
//! backend must share: idempotent collection creation, upsert-then-search round
//! trips, descending-score ordering bounded by `top_k`, deletion by ID,
//! empty-input no-ops, metadata preservation, collection isolation, and
//! collection teardown. Each backend's `vector_store_contract_*.rs` test file
//! constructs a fresh store and runs the suite, so the backends stay
//! behaviorally interchangeable.
//!
//! The module also exposes proptest strategies and [`check_search_invariants`]
//! so backends with a cheap embedded mode can run the search invariants as a
//! property test.
//!
//! The contract asserts relative score ordering only, never absolute score
//! values: InMemory scores are cosine similarity, SurrealDB scores are
//! `1 - cosine distance`, and LanceDB scores are `1 - L2 distance`, so absolute
//! values are not comparable across backends.

// Each backend's test binary uses a subset of these helpers (e.g. the LanceDB
// binary skips the proptest pieces), so the unused remainder is expected.
#![allow(dead_code)]

use std::collections::{BTreeSet, HashMap};

use adk_rag::document::{Chunk, SearchResult};
use adk_rag::vectorstore::VectorStore;
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

const DIM: usize = 4;

/// Backend-specific deviations from the shared contract.
///
/// Every field scopes out one assertion for backends whose behavior diverges.
/// Divergences are documented here rather than fixed, because the contract
/// suite is test-only.
pub struct ContractOptions {
    /// Whether `upsert` with an existing chunk ID replaces the stored row.
    ///
    /// InMemory and SurrealDB replace by ID. LanceDB appends a new row instead
    /// (`upsert` maps to `table.add`), so a repeated ID yields duplicate rows.
    /// Its suite sets this to `false` to scope the replacement assertion out.
    pub upsert_replaces_by_id: bool,
}

impl Default for ContractOptions {
    fn default() -> Self {
        Self { upsert_replaces_by_id: true }
    }
}

/// Build a chunk with the shared contract metadata.
fn chunk(id: &str, text: &str, embedding: Vec<f32>) -> Chunk {
    Chunk {
        id: id.to_string(),
        text: text.to_string(),
        embedding,
        metadata: HashMap::from([("source".to_string(), "contract".to_string())]),
        document_id: "doc_1".to_string(),
    }
}

/// Mirror a chunk into the shape backends are allowed to return.
///
/// Backends may omit the stored vector from search results (LanceDB and
/// SurrealDB return an empty `embedding`; InMemory echoes the stored one), so
/// whole-object comparisons clear the embedding on both sides. The embedding
/// itself is checked separately by [`assert_round_trip`].
fn without_embedding(c: &Chunk) -> Chunk {
    Chunk { embedding: vec![], ..c.clone() }
}

/// Assert a returned chunk matches its stored form.
///
/// Compares the whole object with the embedding cleared, then requires any
/// non-empty returned embedding to equal the stored one.
fn assert_round_trip(returned: &Chunk, stored: &Chunk) {
    assert_eq!(without_embedding(returned), without_embedding(stored));
    if !returned.embedding.is_empty() {
        assert_eq!(returned.embedding, stored.embedding, "echoed embedding must match stored");
    }
}

fn ids(results: &[SearchResult]) -> Vec<String> {
    results.iter().map(|r| r.chunk.id.clone()).collect()
}

fn sorted_ids(results: &[SearchResult]) -> Vec<String> {
    let mut v = ids(results);
    v.sort();
    v
}

fn assert_scores_descending(results: &[SearchResult]) {
    for pair in results.windows(2) {
        assert!(
            pair[0].score >= pair[1].score,
            "scores not in descending order: {} then {}",
            pair[0].score,
            pair[1].score,
        );
    }
}

/// Run the full example-based contract against one store.
///
/// The store must be empty; each scenario uses its own collection name so the
/// checks stay independent.
pub async fn assert_vector_store_contract(store: &dyn VectorStore, options: ContractOptions) {
    // ── create_collection is idempotent and preserves existing rows ──
    store.create_collection("contract_create", DIM).await.expect("create collection");
    store.create_collection("contract_create", DIM).await.expect("repeated create is a no-op");

    let survivor = chunk("survivor", "survives re-create", vec![1.0, 0.0, 0.0, 0.0]);
    store.upsert("contract_create", std::slice::from_ref(&survivor)).await.expect("upsert");
    store.create_collection("contract_create", DIM).await.expect("create after upsert");

    let results = store.search("contract_create", &[1.0, 0.0, 0.0, 0.0], 5).await.expect("search");
    assert_eq!(results.len(), 1, "existing rows must survive a repeated create_collection");
    assert_round_trip(&results[0].chunk, &survivor);

    // ── searching an empty collection returns no results ──
    store.create_collection("contract_empty", DIM).await.expect("create collection");
    let results = store.search("contract_empty", &[1.0, 0.0, 0.0, 0.0], 5).await.expect("search");
    assert!(results.is_empty(), "empty collection must yield no results, got {results:?}");

    // ── upsert-then-search round trip: nearest first, full recall ──
    store.create_collection("contract_search", DIM).await.expect("create collection");
    let alpha = chunk("alpha", "alpha text", vec![1.0, 0.0, 0.0, 0.0]);
    let beta = chunk("beta", "beta text", vec![0.0, 1.0, 0.0, 0.0]);
    let gamma = chunk("gamma", "gamma text", vec![0.0, 0.0, 1.0, 0.0]);
    let stored = vec![alpha.clone(), beta.clone(), gamma.clone()];
    store.upsert("contract_search", &stored).await.expect("upsert");

    let results = store.search("contract_search", &[0.0, 1.0, 0.0, 0.0], 10).await.expect("search");
    assert_eq!(results.len(), 3, "top_k above the row count must return every row");
    assert_round_trip(&results[0].chunk, &beta);
    assert_eq!(
        sorted_ids(&results),
        vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
        "every stored chunk must be retrievable",
    );

    // ── ordering follows similarity and top_k bounds the result count ──
    store.create_collection("contract_order", DIM).await.expect("create collection");
    // Unit vectors at strictly increasing angle from the query [1,0,0,0]. Cosine
    // similarity decreases and L2 distance increases monotonically along the
    // sequence, so the expected order holds for every backend's score metric.
    let ordered = vec![
        chunk("nearest", "nearest", vec![1.0, 0.0, 0.0, 0.0]),
        chunk("near", "near", vec![0.8, 0.6, 0.0, 0.0]),
        chunk("far", "far", vec![0.6, 0.8, 0.0, 0.0]),
        chunk("farthest", "farthest", vec![0.0, 1.0, 0.0, 0.0]),
    ];
    store.upsert("contract_order", &ordered).await.expect("upsert");

    let query = [1.0, 0.0, 0.0, 0.0];
    let results = store.search("contract_order", &query, 10).await.expect("search");
    assert_eq!(ids(&results), vec!["nearest", "near", "far", "farthest"]);
    assert_scores_descending(&results);
    assert!(
        results[0].score > results[results.len() - 1].score,
        "distinct similarities must yield distinct scores, got {:?}",
        results.iter().map(|r| r.score).collect::<Vec<_>>(),
    );

    let bounded = store.search("contract_order", &query, 2).await.expect("search");
    assert_eq!(ids(&bounded), vec!["nearest", "near"], "top_k must keep the best-ranked rows");

    // ── upsert with an existing ID replaces the stored row ──
    if options.upsert_replaces_by_id {
        store.create_collection("contract_replace", DIM).await.expect("create collection");
        let v1 = chunk("dup", "version one", vec![1.0, 0.0, 0.0, 0.0]);
        store.upsert("contract_replace", &[v1]).await.expect("upsert v1");
        let v2 = chunk("dup", "version two", vec![1.0, 0.0, 0.0, 0.0]);
        store.upsert("contract_replace", std::slice::from_ref(&v2)).await.expect("upsert v2");

        let results =
            store.search("contract_replace", &[1.0, 0.0, 0.0, 0.0], 10).await.expect("search");
        assert_eq!(results.len(), 1, "re-upserting an ID must not duplicate the row");
        assert_round_trip(&results[0].chunk, &v2);
    }

    // ── delete removes exactly the named IDs; unknown IDs are a no-op ──
    store.create_collection("contract_delete", DIM).await.expect("create collection");
    let keep_1 = chunk("keep_1", "keep one", vec![1.0, 0.0, 0.0, 0.0]);
    let drop_1 = chunk("drop_1", "drop one", vec![0.0, 1.0, 0.0, 0.0]);
    let keep_2 = chunk("keep_2", "keep two", vec![0.0, 0.0, 1.0, 0.0]);
    store
        .upsert("contract_delete", &[keep_1.clone(), drop_1.clone(), keep_2.clone()])
        .await
        .expect("upsert");

    store.delete("contract_delete", &["drop_1", "no_such_id"]).await.expect("delete");

    let results = store.search("contract_delete", &[1.0, 0.0, 0.0, 0.0], 10).await.expect("search");
    assert_eq!(
        sorted_ids(&results),
        vec!["keep_1".to_string(), "keep_2".to_string()],
        "delete must remove exactly the named IDs",
    );

    // ── empty-input upsert and delete succeed and change nothing ──
    store.upsert("contract_delete", &[]).await.expect("empty upsert is a no-op");
    store.delete("contract_delete", &[]).await.expect("empty delete is a no-op");
    let after = store.search("contract_delete", &[1.0, 0.0, 0.0, 0.0], 10).await.expect("search");
    assert_eq!(
        sorted_ids(&after),
        vec!["keep_1".to_string(), "keep_2".to_string()],
        "empty-input calls must not change stored rows",
    );

    // ── metadata round-trips, including non-trivial values ──
    store.create_collection("contract_meta", DIM).await.expect("create collection");
    let mut carrier = chunk("meta_1", "metadata carrier", vec![1.0, 0.0, 0.0, 0.0]);
    carrier.metadata.insert("page".to_string(), "12".to_string());
    carrier.metadata.insert("quoted".to_string(), r#"he said "hello" \ then left"#.to_string());
    carrier.metadata.insert("unicode".to_string(), "naïve café — 日本語 🚀".to_string());
    carrier.metadata.insert("newlines".to_string(), "line1\nline2\ttabbed".to_string());
    store.upsert("contract_meta", std::slice::from_ref(&carrier)).await.expect("upsert");

    let results = store.search("contract_meta", &[1.0, 0.0, 0.0, 0.0], 1).await.expect("search");
    assert_eq!(results.len(), 1);
    assert_round_trip(&results[0].chunk, &carrier);

    // ── collections are isolated from each other ──
    store.create_collection("contract_iso_a", DIM).await.expect("create collection");
    store.create_collection("contract_iso_b", DIM).await.expect("create collection");
    let only_a = chunk("only_a", "lives in a", vec![1.0, 0.0, 0.0, 0.0]);
    store.upsert("contract_iso_a", std::slice::from_ref(&only_a)).await.expect("upsert");

    let in_b = store.search("contract_iso_b", &[1.0, 0.0, 0.0, 0.0], 10).await.expect("search");
    assert!(in_b.is_empty(), "rows must not leak across collections, got {in_b:?}");
    let in_a = store.search("contract_iso_a", &[1.0, 0.0, 0.0, 0.0], 10).await.expect("search");
    assert_eq!(in_a.len(), 1);
    assert_round_trip(&in_a[0].chunk, &only_a);

    // ── delete_collection makes the data unreachable ──
    store.create_collection("contract_drop", DIM).await.expect("create collection");
    store
        .upsert("contract_drop", &[chunk("gone", "dropped", vec![1.0, 0.0, 0.0, 0.0])])
        .await
        .expect("upsert");
    store.delete_collection("contract_drop").await.expect("delete collection");

    // Backends diverge on a dropped collection: InMemory and LanceDB fail the
    // search, SurrealDB answers with no rows. The shared contract only requires
    // that the data is gone, so an error is accepted as-is.
    if let Ok(results) = store.search("contract_drop", &[1.0, 0.0, 0.0, 0.0], 5).await {
        assert!(results.is_empty(), "dropped collection must hold no rows, got {results:?}");
    }
}

/// Generate a non-zero L2-normalized embedding of the given dimension.
pub fn arb_normalized_embedding(dim: usize) -> impl Strategy<Value = Vec<f32>> {
    proptest::collection::vec(-1.0f32..1.0f32, dim).prop_filter_map(
        "non-zero embedding",
        |mut v| {
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm < 1e-8 {
                return None;
            }
            for val in &mut v {
                *val /= norm;
            }
            Some(v)
        },
    )
}

/// Generate chunks with unique IDs and normalized embeddings.
pub fn arb_unique_chunks(dim: usize, max: usize) -> impl Strategy<Value = Vec<Chunk>> {
    proptest::collection::hash_map(
        "[a-z]{3,8}",
        ("[a-z ]{5,30}", arb_normalized_embedding(dim)),
        1..max,
    )
    .prop_map(|by_id| {
        by_id
            .into_iter()
            .map(|(id, (text, embedding))| Chunk {
                id,
                text,
                embedding,
                metadata: HashMap::new(),
                document_id: "doc_1".to_string(),
            })
            .collect()
    })
}

/// Check the search invariants for one generated case.
///
/// *For any* set of uniquely-identified chunks upserted into a fresh
/// collection and any non-zero query embedding:
/// - `search` returns at most `min(top_k, chunk count)` results,
/// - every returned ID is a stored ID and no ID repeats,
/// - scores are ordered descending,
/// - searching with `top_k` equal to the chunk count returns exactly the
///   stored ID set (every inserted chunk is retrievable).
pub async fn check_search_invariants(
    store: &dyn VectorStore,
    collection: &str,
    chunks: &[Chunk],
    query: &[f32],
    top_k: usize,
) -> Result<(), TestCaseError> {
    let fail =
        |op: &str, e: adk_rag::error::RagError| TestCaseError::fail(format!("{op} failed: {e}"));

    store.create_collection(collection, query.len()).await.map_err(|e| fail("create", e))?;
    store.upsert(collection, chunks).await.map_err(|e| fail("upsert", e))?;

    let stored_ids: BTreeSet<String> = chunks.iter().map(|c| c.id.clone()).collect();

    let results = store.search(collection, query, top_k).await.map_err(|e| fail("search", e))?;
    prop_assert!(results.len() <= top_k, "{} results exceed top_k {top_k}", results.len());
    prop_assert!(results.len() <= chunks.len());

    let returned: Vec<String> = results.iter().map(|r| r.chunk.id.clone()).collect();
    let returned_set: BTreeSet<String> = returned.iter().cloned().collect();
    prop_assert_eq!(returned_set.len(), returned.len(), "returned IDs must be distinct");
    prop_assert!(
        returned_set.is_subset(&stored_ids),
        "returned IDs {returned_set:?} not among stored {stored_ids:?}",
    );

    for pair in results.windows(2) {
        prop_assert!(
            pair[0].score >= pair[1].score,
            "scores not in descending order: {} then {}",
            pair[0].score,
            pair[1].score,
        );
    }

    let all = store.search(collection, query, chunks.len()).await.map_err(|e| fail("search", e))?;
    let all_ids: BTreeSet<String> = all.iter().map(|r| r.chunk.id.clone()).collect();
    prop_assert_eq!(all_ids, stored_ids, "every inserted chunk must be retrievable");

    Ok(())
}
