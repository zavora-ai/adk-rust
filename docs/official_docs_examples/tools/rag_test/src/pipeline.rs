//! RAG doc-test — validates pipeline examples from rag.md

use std::collections::HashMap;
use std::sync::Arc;

use adk_rag::{
    Document, EmbeddingProvider, FixedSizeChunker, InMemoryVectorStore, RagConfig, RagPipeline,
};

struct MockEmbedder;

#[async_trait::async_trait]
impl EmbeddingProvider for MockEmbedder {
    async fn embed(&self, text: &str) -> adk_rag::Result<Vec<f32>> {
        let hash = text.bytes().fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
        let mut v = vec![0.0f32; 64];
        for (i, x) in v.iter_mut().enumerate() {
            *x = ((hash.wrapping_add(i as u64)) as f32).sin();
        }
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            v.iter_mut().for_each(|x| *x /= norm);
        }
        Ok(v)
    }
    fn dimensions(&self) -> usize {
        64
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== RAG Pipeline Doc-Test ===\n");

    // From docs: Build a pipeline
    let pipeline = RagPipeline::builder()
        .config(RagConfig::builder().chunk_size(256).chunk_overlap(50).top_k(3).build()?)
        .embedding_provider(Arc::new(MockEmbedder))
        .vector_store(Arc::new(InMemoryVectorStore::new()))
        .chunker(Arc::new(FixedSizeChunker::new(256, 50)))
        .build()?;
    println!("✓ Pipeline builder works");

    // From docs: Create collection and ingest
    pipeline.create_collection("docs").await?;
    println!("✓ create_collection works");

    let chunks = pipeline
        .ingest(
            "docs",
            &Document {
                id: "intro".to_string(),
                text: "Rust is a systems programming language focused on safety and speed."
                    .to_string(),
                metadata: HashMap::from([("topic".to_string(), "rust".to_string())]),
                source_uri: None,
            },
        )
        .await?;
    assert!(!chunks.is_empty());
    println!("✓ ingest works — {} chunks", chunks.len());

    // From docs: Query
    let results = pipeline.query("docs", "safe programming").await?;
    assert!(!results.is_empty());
    println!("✓ query works — {} results", results.len());

    for r in &results {
        assert!(r.score.is_finite());
        assert!(!r.chunk.text.is_empty());
    }
    println!("✓ results have valid scores and text");

    // From docs: Batch ingest
    let docs = vec![
        Document {
            id: "doc2".to_string(),
            text: "Python is great for data science.".to_string(),
            metadata: Default::default(),
            source_uri: None,
        },
        Document {
            id: "doc3".to_string(),
            text: "JavaScript runs in the browser.".to_string(),
            metadata: Default::default(),
            source_uri: None,
        },
    ];
    let all_chunks = pipeline.ingest_batch("docs", &docs).await?;
    assert!(all_chunks.len() >= 2);
    println!("✓ ingest_batch works — {} chunks", all_chunks.len());

    // From docs: Delete collection
    pipeline.delete_collection("docs").await?;
    println!("✓ delete_collection works");

    println!("\n=== All pipeline tests passed! ===");
    Ok(())
}
