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
