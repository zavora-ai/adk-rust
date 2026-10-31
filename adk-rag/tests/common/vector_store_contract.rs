//! Shared behavioral contract for [`VectorStore`] backends.
//!
//! [`assert_vector_store_contract`] walks one store through the semantics every
//! backend must share: idempotent collection creation, upsert-then-search round
//! trips, descending-score ordering bounded by `top_k`, deletion by ID,
//! empty-input no-ops, metadata and provenance preservation, collection isolation, distinct
//! names never sharing a collection, scope isolation (one chunk ID in two scopes, delete,
//! count, and purge per scope, and a small scope's recall beside a large nearer one), and
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

use std::collections::BTreeSet;

use adk_rag::Scope;
use adk_rag::document::{Chunk, SearchResult};
use adk_rag::vectorstore::{CollectionSpec, VectorQuery, VectorStore};
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
    /// InMemory, SurrealDB, and LanceDB (through a merge-insert keyed on the
    /// id) all replace by ID, so every current suite leaves this `true`.
    pub upsert_replaces_by_id: bool,
}

impl Default for ContractOptions {
    fn default() -> Self {
        Self { upsert_replaces_by_id: true }
    }
}

fn spec() -> CollectionSpec {
    CollectionSpec::new(DIM)
}

/// Build a chunk with the shared contract metadata.
fn chunk(id: &str, text: &str, embedding: Vec<f32>) -> Chunk {
    Chunk::new(id, "doc_1", text).with_embedding(embedding).with_metadata("source", "contract")
}

/// Mirror a chunk into the shape backends are allowed to return.
///
/// Backends may omit the stored vector from search results (LanceDB and
/// SurrealDB return an empty `embedding`; InMemory echoes the stored one), so
/// whole-object comparisons clear the embedding on both sides. The embedding
/// itself is checked separately by [`assert_round_trip`].
fn without_embedding(c: &Chunk) -> Chunk {
    c.clone().with_embedding(vec![])
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
    let scope = Scope::from_parts(["contract", "main"]).expect("valid scope");

    // ── create_collection is idempotent and preserves existing rows ──
    store.create_collection("contract_create", &spec()).await.expect("create collection");
    store.create_collection("contract_create", &spec()).await.expect("repeated create is a no-op");

    let survivor = chunk("survivor", "survives re-create", vec![1.0, 0.0, 0.0, 0.0]);
    store.upsert("contract_create", &scope, std::slice::from_ref(&survivor)).await.expect("upsert");
    store.create_collection("contract_create", &spec()).await.expect("create after upsert");

    let results = store
        .search("contract_create", &scope, &VectorQuery::new(&[1.0, 0.0, 0.0, 0.0], 5))
        .await
        .expect("search");
    assert_eq!(results.len(), 1, "existing rows must survive a repeated create_collection");
    assert_round_trip(&results[0].chunk, &survivor);

    // ── searching an empty collection returns no results ──
    store.create_collection("contract_empty", &spec()).await.expect("create collection");
    let results = store
        .search("contract_empty", &scope, &VectorQuery::new(&[1.0, 0.0, 0.0, 0.0], 5))
        .await
        .expect("search");
    assert!(results.is_empty(), "empty collection must yield no results, got {results:?}");

    // ── upsert-then-search round trip: nearest first, full recall ──
    store.create_collection("contract_search", &spec()).await.expect("create collection");
    let alpha = chunk("alpha", "alpha text", vec![1.0, 0.0, 0.0, 0.0]);
    let beta = chunk("beta", "beta text", vec![0.0, 1.0, 0.0, 0.0]);
    let gamma = chunk("gamma", "gamma text", vec![0.0, 0.0, 1.0, 0.0]);
    let stored = vec![alpha.clone(), beta.clone(), gamma.clone()];
    store.upsert("contract_search", &scope, &stored).await.expect("upsert");

    let results = store
        .search("contract_search", &scope, &VectorQuery::new(&[0.0, 1.0, 0.0, 0.0], 10))
        .await
        .expect("search");
    assert_eq!(results.len(), 3, "top_k above the row count must return every row");
    assert_round_trip(&results[0].chunk, &beta);
    assert_eq!(
        sorted_ids(&results),
        vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
        "every stored chunk must be retrievable",
    );

    // ── ordering follows similarity and top_k bounds the result count ──
    store.create_collection("contract_order", &spec()).await.expect("create collection");
    // Unit vectors at strictly increasing angle from the query [1,0,0,0]. Cosine
    // similarity decreases and L2 distance increases monotonically along the
    // sequence, so the expected order holds for every backend's score metric.
    let ordered = vec![
        chunk("nearest", "nearest", vec![1.0, 0.0, 0.0, 0.0]),
        chunk("near", "near", vec![0.8, 0.6, 0.0, 0.0]),
        chunk("far", "far", vec![0.6, 0.8, 0.0, 0.0]),
        chunk("farthest", "farthest", vec![0.0, 1.0, 0.0, 0.0]),
    ];
    store.upsert("contract_order", &scope, &ordered).await.expect("upsert");

    let query = [1.0, 0.0, 0.0, 0.0];
    let results = store
        .search("contract_order", &scope, &VectorQuery::new(&query, 10))
        .await
        .expect("search");
    assert_eq!(ids(&results), vec!["nearest", "near", "far", "farthest"]);
    assert_scores_descending(&results);
    assert!(
        results[0].score > results[results.len() - 1].score,
        "distinct similarities must yield distinct scores, got {:?}",
        results.iter().map(|r| r.score).collect::<Vec<_>>(),
    );

    let bounded =
        store.search("contract_order", &scope, &VectorQuery::new(&query, 2)).await.expect("search");
    assert_eq!(ids(&bounded), vec!["nearest", "near"], "top_k must keep the best-ranked rows");

    // ── upsert with an existing ID replaces the stored row ──
    if options.upsert_replaces_by_id {
        store.create_collection("contract_replace", &spec()).await.expect("create collection");
        let v1 = chunk("dup", "version one", vec![1.0, 0.0, 0.0, 0.0]);
        store.upsert("contract_replace", &scope, &[v1]).await.expect("upsert v1");
        let v2 = chunk("dup", "version two", vec![1.0, 0.0, 0.0, 0.0]);
        store
            .upsert("contract_replace", &scope, std::slice::from_ref(&v2))
            .await
            .expect("upsert v2");

        let results = store
            .search("contract_replace", &scope, &VectorQuery::new(&[1.0, 0.0, 0.0, 0.0], 10))
            .await
            .expect("search");
        assert_eq!(results.len(), 1, "re-upserting an ID must not duplicate the row");
        assert_round_trip(&results[0].chunk, &v2);
    }

    // ── delete removes exactly the named IDs; unknown IDs are a no-op ──
    store.create_collection("contract_delete", &spec()).await.expect("create collection");
    let keep_1 = chunk("keep_1", "keep one", vec![1.0, 0.0, 0.0, 0.0]);
    let drop_1 = chunk("drop_1", "drop one", vec![0.0, 1.0, 0.0, 0.0]);
    let keep_2 = chunk("keep_2", "keep two", vec![0.0, 0.0, 1.0, 0.0]);
    store
        .upsert("contract_delete", &scope, &[keep_1.clone(), drop_1.clone(), keep_2.clone()])
        .await
        .expect("upsert");

    store.delete("contract_delete", &scope, &["drop_1", "no_such_id"]).await.expect("delete");

    let results = store
        .search("contract_delete", &scope, &VectorQuery::new(&[1.0, 0.0, 0.0, 0.0], 10))
        .await
        .expect("search");
    assert_eq!(
        sorted_ids(&results),
        vec!["keep_1".to_string(), "keep_2".to_string()],
        "delete must remove exactly the named IDs",
    );

    // ── empty-input upsert and delete succeed and change nothing ──
    store.upsert("contract_delete", &scope, &[]).await.expect("empty upsert is a no-op");
    store.delete("contract_delete", &scope, &[]).await.expect("empty delete is a no-op");
    let after = store
        .search("contract_delete", &scope, &VectorQuery::new(&[1.0, 0.0, 0.0, 0.0], 10))
        .await
        .expect("search");
    assert_eq!(
        sorted_ids(&after),
        vec!["keep_1".to_string(), "keep_2".to_string()],
        "empty-input calls must not change stored rows",
    );

    // ── metadata round-trips, including non-trivial values ──
    store.create_collection("contract_meta", &spec()).await.expect("create collection");
    let mut carrier = chunk("meta_1", "metadata carrier", vec![1.0, 0.0, 0.0, 0.0]);
    carrier.metadata.insert("page".to_string(), "12".to_string());
    carrier.metadata.insert("quoted".to_string(), r#"he said "hello" \ then left"#.to_string());
    carrier.metadata.insert("unicode".to_string(), "naïve café — 日本語 🚀".to_string());
    carrier.metadata.insert("newlines".to_string(), "line1\nline2\ttabbed".to_string());
    store.upsert("contract_meta", &scope, std::slice::from_ref(&carrier)).await.expect("upsert");

    let results = store
        .search("contract_meta", &scope, &VectorQuery::new(&[1.0, 0.0, 0.0, 0.0], 1))
        .await
        .expect("search");
    assert_eq!(results.len(), 1);
    assert_round_trip(&results[0].chunk, &carrier);

    // ── source URI and title round-trip as fields of their own ──
    store.create_collection("contract_provenance", &spec()).await.expect("create collection");
    let cited = chunk("cited", "cited text", vec![1.0, 0.0, 0.0, 0.0])
        .with_source_uri("https://example.com/docs/a?page=2&lang=en#intro")
        .with_title("Guide — naïve café 日本語");
    let uncited = chunk("uncited", "uncited text", vec![0.0, 1.0, 0.0, 0.0]);
    store
        .upsert("contract_provenance", &scope, &[cited.clone(), uncited.clone()])
        .await
        .expect("upsert");

    let results = store
        .search("contract_provenance", &scope, &VectorQuery::new(&[1.0, 0.0, 0.0, 0.0], 10))
        .await
        .expect("search");
    assert_eq!(ids(&results), ["cited", "uncited"]);
    assert_round_trip(&results[0].chunk, &cited);
    assert_round_trip(&results[1].chunk, &uncited);
    assert!(
        !results[0].chunk.metadata.contains_key("source_uri")
            && !results[0].chunk.metadata.contains_key("title"),
        "provenance must not be folded into metadata"
    );

    // ── collections are isolated from each other ──
    store.create_collection("contract_iso_a", &spec()).await.expect("create collection");
    store.create_collection("contract_iso_b", &spec()).await.expect("create collection");
    let only_a = chunk("only_a", "lives in a", vec![1.0, 0.0, 0.0, 0.0]);
    store.upsert("contract_iso_a", &scope, std::slice::from_ref(&only_a)).await.expect("upsert");

    let in_b = store
        .search("contract_iso_b", &scope, &VectorQuery::new(&[1.0, 0.0, 0.0, 0.0], 10))
        .await
        .expect("search");
    assert!(in_b.is_empty(), "rows must not leak across collections, got {in_b:?}");
    let in_a = store
        .search("contract_iso_a", &scope, &VectorQuery::new(&[1.0, 0.0, 0.0, 0.0], 10))
        .await
        .expect("search");
    assert_eq!(in_a.len(), 1);
    assert_round_trip(&in_a[0].chunk, &only_a);

    // ── distinct names never share a collection ──
    // A backend accepts a name only if it maps to a collection of its own; a name it
    // would have to rewrite onto another (`contract-alias` onto `contract_alias`) is
    // refused instead.
    let mut accepted = Vec::new();
    for name in ["contract_alias", "contract-alias", "contract alias", "contract.alias"] {
        if store.create_collection(name, &spec()).await.is_ok() {
            accepted.push(name);
        }
    }
    assert_eq!(accepted.first(), Some(&"contract_alias"), "every backend accepts `contract_alias`");
    for (i, name) in accepted.iter().enumerate() {
        let own = chunk(&format!("alias_{i}"), name, vec![1.0, 0.0, 0.0, 0.0]);
        store.upsert(name, &scope, &[own]).await.expect("upsert into an accepted collection");
    }
    for (i, name) in accepted.iter().enumerate() {
        let results = store
            .search(name, &scope, &VectorQuery::new(&[1.0, 0.0, 0.0, 0.0], 10))
            .await
            .expect("search");
        assert_eq!(ids(&results), [format!("alias_{i}")], "collection {name:?} shares rows");
    }

    // ── scopes: a chunk is identified by collection, scope, and chunk ID ──
    store.create_collection("contract_scopes", &spec()).await.expect("create collection");
    let tenant_a = Scope::from_parts(["contract", "tenant a"]).expect("valid scope");
    let tenant_b = Scope::from_parts(["contract", "tenant b"]).expect("valid scope");
    let a_doc = chunk("shared", "tenant a's text", vec![1.0, 0.0, 0.0, 0.0]);
    let b_doc = chunk("shared", "tenant b's text", vec![1.0, 0.0, 0.0, 0.0]);
    let b_other = chunk("b_only", "only in b", vec![0.0, 1.0, 0.0, 0.0]);
    store
        .upsert("contract_scopes", &tenant_a, std::slice::from_ref(&a_doc))
        .await
        .expect("upsert a");
    store
        .upsert("contract_scopes", &tenant_b, &[b_doc.clone(), b_other.clone()])
        .await
        .expect("upsert b");

    let query = VectorQuery::new(&[1.0, 0.0, 0.0, 0.0], 10);
    let in_a = store.search("contract_scopes", &tenant_a, &query).await.expect("search a");
    assert_eq!(ids(&in_a), ["shared"], "scope a sees only its own chunk");
    assert_round_trip(&in_a[0].chunk, &a_doc);
    let in_b = store.search("contract_scopes", &tenant_b, &query).await.expect("search b");
    assert_eq!(ids(&in_b), ["shared", "b_only"], "the same chunk ID in b is b's own row");
    assert_round_trip(&in_b[0].chunk, &b_doc);
    let unused = Scope::new("contract never written").expect("valid scope");
    let in_unused = store.search("contract_scopes", &unused, &query).await.expect("search");
    assert!(in_unused.is_empty(), "an unwritten scope holds nothing, got {in_unused:?}");

    assert_eq!(store.count("contract_scopes", &tenant_a).await.expect("count a"), 1);
    assert_eq!(store.count("contract_scopes", &tenant_b).await.expect("count b"), 2);
    assert_eq!(store.count("contract_scopes", &unused).await.expect("count"), 0);

    // delete in one scope leaves the other scope's chunk with the same ID
    store.delete("contract_scopes", &tenant_a, &["shared"]).await.expect("delete in a");
    assert!(store.search("contract_scopes", &tenant_a, &query).await.expect("search").is_empty());
    let in_b = store.search("contract_scopes", &tenant_b, &query).await.expect("search b");
    assert_eq!(ids(&in_b), ["shared", "b_only"], "delete in a must not touch b");

    // purge_scope empties one scope only and reports how many chunks it held
    store
        .upsert("contract_scopes", &tenant_a, std::slice::from_ref(&a_doc))
        .await
        .expect("upsert a again");
    assert_eq!(store.purge_scope("contract_scopes", &tenant_b).await.expect("purge b"), 2);
    assert_eq!(store.count("contract_scopes", &tenant_b).await.expect("count b"), 0);
    assert!(store.search("contract_scopes", &tenant_b, &query).await.expect("search").is_empty());
    assert_eq!(store.count("contract_scopes", &tenant_a).await.expect("count a"), 1);
    assert_eq!(store.purge_scope("contract_scopes", &unused).await.expect("purge"), 0);

    // ── a small scope keeps its recall beside a large, nearer one ──
    // The scope must apply inside the search, before the nearest rows are chosen: a search
    // that took the global top k and then dropped other scopes' rows would return nothing.
    store.create_collection("contract_recall", &spec()).await.expect("create collection");
    let small = Scope::from_parts(["contract", "small"]).expect("valid scope");
    let large = Scope::from_parts(["contract", "large"]).expect("valid scope");
    let lone = chunk("lone", "the small scope's only chunk", vec![0.0, 0.0, 0.0, 1.0]);
    store.upsert("contract_recall", &small, std::slice::from_ref(&lone)).await.expect("upsert");
    let crowd: Vec<Chunk> = (0..500)
        .map(|i| {
            let jitter = i as f32 / 5000.0;
            chunk(&format!("crowd_{i}"), "nearer", vec![1.0, jitter, 0.0, 0.1])
        })
        .collect();
    store.upsert("contract_recall", &large, &crowd).await.expect("upsert crowd");

    let nearest = VectorQuery::new(&[1.0, 0.0, 0.0, 0.0], 1);
    let found = store.search("contract_recall", &small, &nearest).await.expect("search");
    assert_eq!(ids(&found), ["lone"], "top_k=1 in the small scope must find its chunk");
    assert_eq!(store.count("contract_recall", &large).await.expect("count"), 500);

    // ── delete_collection makes the data unreachable ──
    store.create_collection("contract_drop", &spec()).await.expect("create collection");
    store
        .upsert("contract_drop", &scope, &[chunk("gone", "dropped", vec![1.0, 0.0, 0.0, 0.0])])
        .await
        .expect("upsert");
    store.delete_collection("contract_drop").await.expect("delete collection");

    // Backends diverge on a dropped collection: InMemory and LanceDB fail the
    // search, SurrealDB answers with no rows. The shared contract only requires
    // that the data is gone, so an error is accepted as-is.
    if let Ok(results) =
        store.search("contract_drop", &scope, &VectorQuery::new(&[1.0, 0.0, 0.0, 0.0], 5)).await
    {
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
            .map(|(id, (text, embedding))| Chunk::new(id, "doc_1", text).with_embedding(embedding))
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

    let scope = Scope::single_tenant();
    store
        .create_collection(collection, &CollectionSpec::new(query.len()))
        .await
        .map_err(|e| fail("create", e))?;
    store.upsert(collection, &scope, chunks).await.map_err(|e| fail("upsert", e))?;

    let stored_ids: BTreeSet<String> = chunks.iter().map(|c| c.id.clone()).collect();

    let results = store
        .search(collection, &scope, &VectorQuery::new(query, top_k))
        .await
        .map_err(|e| fail("search", e))?;
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

    let all = store
        .search(collection, &scope, &VectorQuery::new(query, chunks.len()))
        .await
        .map_err(|e| fail("search", e))?;
    let all_ids: BTreeSet<String> = all.iter().map(|r| r.chunk.id.clone()).collect();
    prop_assert_eq!(all_ids, stored_ids, "every inserted chunk must be retrievable");

    Ok(())
}
