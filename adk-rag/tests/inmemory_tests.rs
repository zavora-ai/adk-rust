//! Property tests for in-memory vector store search ordering.

use std::collections::HashMap;

use adk_rag::document::Chunk;
use adk_rag::inmemory::InMemoryVectorStore;
use adk_rag::{CollectionSpec, Scope, VectorQuery, VectorStore};
use proptest::prelude::*;

/// Generate a non-zero L2-normalized embedding of the given dimension.
fn arb_normalized_embedding(dim: usize) -> impl Strategy<Value = Vec<f32>> {
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

/// Generate a chunk with a normalized embedding.
fn arb_chunk(dim: usize) -> impl Strategy<Value = Chunk> {
    ("[a-z]{3,8}", "[a-z ]{5,30}", arb_normalized_embedding(dim))
        .prop_map(|(id, text, embedding)| Chunk::new(id, "doc_1", text).with_embedding(embedding))
}

/// **Feature: adk-rag, Property 3: In-memory vector store search ordering**
/// *For any* set of chunks with embeddings stored in an InMemoryVectorStore,
/// searching with a query embedding SHALL return results ordered by descending
/// cosine similarity score, and the number of results SHALL be at most top_k.
/// **Validates: Requirements 2.3**
mod prop_inmemory_search_ordering {
    use super::*;

    const DIM: usize = 16;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn results_ordered_descending_and_bounded_by_top_k(
            chunks in proptest::collection::vec(arb_chunk(DIM), 1..20),
            query in arb_normalized_embedding(DIM),
            top_k in 1usize..25,
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let results = rt.block_on(async {
                let store = InMemoryVectorStore::new();
                store.create_collection("test", &CollectionSpec::new(DIM)).await.unwrap();

                // Deduplicate chunks by id to avoid upsert overwriting
                let mut deduped: HashMap<String, Chunk> = HashMap::new();
                for chunk in &chunks {
                    deduped.entry(chunk.id.clone()).or_insert_with(|| chunk.clone());
                }
                let unique_chunks: Vec<Chunk> = deduped.into_values().collect();
                let count = unique_chunks.len();

                store.upsert("test", &Scope::single_tenant(), &unique_chunks).await.unwrap();
                let results = store.search("test", &Scope::single_tenant(), &VectorQuery::new(&query, top_k)).await.unwrap();
                (results, count)
            });

            let (results, unique_count) = results;

            // Result count is at most top_k and at most the number of stored chunks
            prop_assert!(results.len() <= top_k);
            prop_assert!(results.len() <= unique_count);

            // Results are ordered by descending score
            for window in results.windows(2) {
                prop_assert!(
                    window[0].score >= window[1].score,
                    "results not in descending order: {} < {}",
                    window[0].score,
                    window[1].score,
                );
            }
        }
    }
}

/// Results of equal score come back in ascending chunk-id order, so a store answers the same
/// query with the same chunks in every process; results of different scores keep score order.
#[tokio::test]
async fn equal_scores_come_back_in_chunk_id_order() {
    let ties: Vec<String> = (0..24).map(|n| format!("tie-{n:02}")).collect();
    let mut chunks: Vec<Chunk> = ties
        .iter()
        .map(|id| {
            Chunk::new(id, "clip", "a forklift crosses the aisle").with_embedding(vec![1.0, 0.0])
        })
        .collect();
    // Their ids sort against their scores, so only score can place them.
    chunks.push(Chunk::new("z-closer", "clip", "a pallet").with_embedding(vec![1.0, 1.0]));
    chunks.push(Chunk::new("a-farthest", "clip", "an empty aisle").with_embedding(vec![0.0, 1.0]));
    chunks.reverse();
    let store = InMemoryVectorStore::new();
    store.create_collection("clips", &CollectionSpec::new(2)).await.unwrap();
    store.upsert("clips", &Scope::single_tenant(), &chunks).await.unwrap();

    let ids = |results: Vec<adk_rag::SearchResult>| -> Vec<String> {
        results.into_iter().map(|result| result.chunk.id).collect()
    };
    let all = store
        .search("clips", &Scope::single_tenant(), &VectorQuery::new(&[1.0, 0.0], 30))
        .await
        .unwrap();
    let mut expected = ties.clone();
    expected.extend(["z-closer".to_string(), "a-farthest".to_string()]);
    assert_eq!(ids(all), expected);

    let top = store
        .search("clips", &Scope::single_tenant(), &VectorQuery::new(&[1.0, 0.0], 5))
        .await
        .unwrap();
    assert_eq!(ids(top), ties[..5]);
}
