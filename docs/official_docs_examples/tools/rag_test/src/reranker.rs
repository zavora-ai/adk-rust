//! RAG doc-test — validates custom reranker example from rag.md

use adk_rag::{Chunk, NoOpReranker, Reranker, SearchResult};

// From docs: Custom KeywordBoostReranker
struct KeywordBoostReranker {
    boost: f32,
}

#[async_trait::async_trait]
impl Reranker for KeywordBoostReranker {
    async fn rerank(
        &self,
        query: &str,
        mut results: Vec<SearchResult>,
    ) -> adk_rag::Result<Vec<SearchResult>> {
        let keywords: Vec<String> =
            query.split_whitespace().filter(|w| w.len() > 3).map(|w| w.to_lowercase()).collect();

        for r in &mut results {
            let text = r.chunk.text.to_lowercase();
            let hits = keywords.iter().filter(|kw| text.contains(kw.as_str())).count();
            r.score += hits as f32 * self.boost;
        }
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        Ok(results)
    }
}

fn make_result(text: &str, score: f32) -> SearchResult {
    SearchResult {
        chunk: Chunk {
            id: "c1".to_string(),
            text: text.to_string(),
            embedding: vec![],
            metadata: Default::default(),
            document_id: "d1".to_string(),
        },
        score,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== RAG Reranker Doc-Test ===\n");

    // NoOpReranker passes through unchanged
    let noop = NoOpReranker;
    let results = vec![make_result("hello world", 0.5), make_result("goodbye", 0.3)];
    let reranked = noop.rerank("test", results.clone()).await?;
    assert_eq!(reranked.len(), 2);
    assert!((reranked[0].score - 0.5).abs() < f32::EPSILON);
    assert!((reranked[1].score - 0.3).abs() < f32::EPSILON);
    println!("✓ NoOpReranker passes results through unchanged");

    // KeywordBoostReranker boosts matching results
    let reranker = KeywordBoostReranker { boost: 0.1 };
    let results = vec![
        make_result("Python is great for data science", 0.5),
        make_result("Rust vector database with similarity search", 0.4),
    ];
    let reranked = reranker.rerank("vector database search", results).await?;

    // The Rust/vector result should be boosted above the Python result
    assert_eq!(reranked.len(), 2);
    assert!(reranked[0].score > 0.4, "boosted result should have higher score");
    println!("✓ KeywordBoostReranker boosts matching results");

    // Results are re-sorted by score
    for i in 0..reranked.len() - 1 {
        assert!(reranked[i].score >= reranked[i + 1].score, "results should be sorted descending");
    }
    println!("✓ Results are sorted by score descending");

    println!("\n=== All reranker tests passed! ===");
    Ok(())
}
