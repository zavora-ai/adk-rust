//! # RAG Basic Example
//!
//! Demonstrates the core RAG pipeline: ingest documents, then query them.
//!
//! Uses `InMemoryVectorStore`, `FixedSizeChunker`, and a deterministic
//! `MockEmbeddingProvider` so it runs with **zero API keys**.
//!
//! Run: `cargo run --example rag_basic --features rag`

use std::collections::HashMap;
use std::sync::Arc;

use adk_rag::{
    Document, EmbeddingProvider, FixedSizeChunker, InMemoryVectorStore, RagConfig, RagPipeline,
};

// ---------------------------------------------------------------------------
// MockEmbeddingProvider — deterministic hash-based embeddings for demos/tests
// ---------------------------------------------------------------------------

struct MockEmbeddingProvider {
    dimensions: usize,
}

impl MockEmbeddingProvider {
    fn new(dimensions: usize) -> Self {
        Self { dimensions }
    }
}

#[async_trait::async_trait]
impl EmbeddingProvider for MockEmbeddingProvider {
    async fn embed(&self, text: &str) -> adk_rag::Result<Vec<f32>> {
        // Deterministic embedding: hash the text bytes, then generate a
        // normalised vector whose direction depends on the content.
        let hash = text.bytes().fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
        let mut emb = vec![0.0f32; self.dimensions];
        for (i, v) in emb.iter_mut().enumerate() {
            *v = ((hash.wrapping_add(i as u64)) as f32).sin();
        }
        // L2-normalise so cosine similarity is just the dot product.
        let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            emb.iter_mut().for_each(|x| *x /= norm);
        }
        Ok(emb)
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // -- 1. Configure the pipeline ----------------------------------------
    // chunk_size=200 keeps chunks small for this demo; overlap=50 ensures
    // context is shared between adjacent chunks; top_k=3 returns the three
    // most relevant results.
    let config = RagConfig::builder()
        .chunk_size(200)
        .chunk_overlap(50)
        .top_k(3)
        .similarity_threshold(0.0)
        .build()?;

    // -- 2. Build the pipeline with in-memory components ------------------
    // MockEmbeddingProvider produces 64-dimensional vectors from text hashes.
    // InMemoryVectorStore stores everything in a HashMap — no external DB.
    // FixedSizeChunker splits text into 200-char chunks with 50-char overlap.
    let pipeline = Arc::new(
        RagPipeline::builder()
            .config(config)
            .embedding_provider(Arc::new(MockEmbeddingProvider::new(64)))
            .vector_store(Arc::new(InMemoryVectorStore::new()))
            .chunker(Arc::new(FixedSizeChunker::new(200, 50)))
            .build()?,
    );

    // -- 3. Create a collection -------------------------------------------
    let collection = "knowledge_base";
    pipeline.create_collection(collection).await?;

    // -- 4. Ingest sample documents ---------------------------------------
    let documents = vec![
        Document {
            id: "doc1".into(),
            text: "Rust is a systems programming language focused on safety, speed, \
                   and concurrency. It achieves memory safety without a garbage collector \
                   through its ownership system."
                .into(),
            metadata: HashMap::from([("topic".into(), "rust".into())]),
            source_uri: Some("https://www.rust-lang.org".into()),
        },
        Document {
            id: "doc2".into(),
            text: "Python is a high-level, interpreted programming language known for \
                   its readability and versatility. It is widely used in data science, \
                   web development, and automation."
                .into(),
            metadata: HashMap::from([("topic".into(), "python".into())]),
            source_uri: Some("https://www.python.org".into()),
        },
        Document {
            id: "doc3".into(),
            text: "Retrieval-Augmented Generation (RAG) combines a retrieval system \
                   with a language model. Documents are chunked, embedded, and stored \
                   in a vector database. At query time the most relevant chunks are \
                   retrieved and fed to the LLM as context."
                .into(),
            metadata: HashMap::from([("topic".into(), "rag".into())]),
            source_uri: None,
        },
    ];

    println!("Ingesting {} documents...", documents.len());
    for doc in &documents {
        let chunks = pipeline.ingest(collection, doc).await?;
        println!("  {} → {} chunk(s)", doc.id, chunks.len());
    }

    // -- 5. Query the pipeline --------------------------------------------
    let queries = ["memory safety in programming", "data science language", "vector database"];

    for query in &queries {
        println!("\nQuery: \"{query}\"");
        let results = pipeline.query(collection, query).await?;
        if results.is_empty() {
            println!("  (no results)");
        } else {
            for (i, result) in results.iter().enumerate() {
                println!(
                    "  {}. [score={:.4}] doc={} | {}",
                    i + 1,
                    result.score,
                    result.chunk.document_id,
                    // Show a short preview of the chunk text.
                    &result.chunk.text[..result.chunk.text.len().min(80)],
                );
            }
        }
    }

    println!("\nDone.");
    Ok(())
}
