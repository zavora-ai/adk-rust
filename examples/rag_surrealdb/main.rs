//! # RAG SurrealDB Example
//!
//! Demonstrates the RAG pipeline using SurrealDB as the vector store.
//!
//! SurrealDB runs in embedded in-memory mode here — no server needed.
//! Uses a deterministic `MockEmbeddingProvider` so it runs with **zero API keys**.
//!
//! This shows the same ingest-and-query workflow as `rag_basic`, but backed
//! by SurrealDB's native HNSW vector indexing and KNN search instead of the
//! simple in-memory HashMap store.
//!
//! Run: `cargo run --example rag_surrealdb --features rag-surrealdb`

use std::collections::HashMap;
use std::sync::Arc;

use adk_rag::{
    Document, EmbeddingProvider, FixedSizeChunker, RagConfig, RagPipeline,
    surrealdb::SurrealVectorStore,
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
        let hash = text.bytes().fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
        let mut emb = vec![0.0f32; self.dimensions];
        for (i, v) in emb.iter_mut().enumerate() {
            *v = ((hash.wrapping_add(i as u64)) as f32).sin();
        }
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
    let config = RagConfig::builder()
        .chunk_size(200)
        .chunk_overlap(50)
        .top_k(3)
        .similarity_threshold(0.0)
        .build()?;

    // -- 2. Create a SurrealDB-backed vector store ------------------------
    // in_memory() starts an embedded SurrealDB engine — no server needed.
    // For persistence, swap to SurrealVectorStore::rocksdb("data/rag").
    // For a remote server, use SurrealVectorStore::remote("ws://localhost:8000").
    let store = SurrealVectorStore::in_memory().await?;
    println!("SurrealDB vector store ready (embedded in-memory mode)");

    // -- 3. Build the pipeline --------------------------------------------
    let pipeline = Arc::new(
        RagPipeline::builder()
            .config(config)
            .embedding_provider(Arc::new(MockEmbeddingProvider::new(64)))
            .vector_store(Arc::new(store))
            .chunker(Arc::new(FixedSizeChunker::new(200, 50)))
            .build()?,
    );

    // -- 4. Create a collection -------------------------------------------
    // This creates a SurrealDB table with an HNSW cosine index.
    let collection = "knowledge_base";
    pipeline.create_collection(collection).await?;
    println!("Created collection '{collection}' with HNSW index\n");

    // -- 5. Ingest sample documents ---------------------------------------
    let documents = vec![
        Document {
            id: "doc1".into(),
            text: "Rust is a systems programming language focused on safety, speed, \
                   and concurrency. It achieves memory safety without a garbage collector \
                   through its ownership system. The borrow checker enforces strict rules \
                   at compile time to prevent data races and dangling pointers."
                .into(),
            metadata: HashMap::from([("topic".into(), "rust".into())]),
            source_uri: Some("https://www.rust-lang.org".into()),
        },
        Document {
            id: "doc2".into(),
            text: "Python is a high-level, interpreted programming language known for \
                   its readability and versatility. It is widely used in data science, \
                   web development, and automation. Libraries like NumPy, pandas, and \
                   scikit-learn make it the go-to language for machine learning."
                .into(),
            metadata: HashMap::from([("topic".into(), "python".into())]),
            source_uri: Some("https://www.python.org".into()),
        },
        Document {
            id: "doc3".into(),
            text: "SurrealDB is a multi-model database written in Rust. It supports \
                   document, graph, and vector data in a single engine. Its native HNSW \
                   indexing enables fast approximate nearest-neighbor search for embeddings. \
                   It can run embedded (in-process) or as a distributed server."
                .into(),
            metadata: HashMap::from([("topic".into(), "surrealdb".into())]),
            source_uri: Some("https://surrealdb.com".into()),
        },
        Document {
            id: "doc4".into(),
            text: "Retrieval-Augmented Generation (RAG) combines a retrieval system \
                   with a language model. Documents are chunked, embedded, and stored \
                   in a vector database. At query time the most relevant chunks are \
                   retrieved and fed to the LLM as context, grounding its answers in \
                   your actual data rather than training knowledge alone."
                .into(),
            metadata: HashMap::from([("topic".into(), "rag".into())]),
            source_uri: None,
        },
    ];

    println!("Ingesting {} documents into SurrealDB...", documents.len());
    for doc in &documents {
        let chunks = pipeline.ingest(collection, doc).await?;
        println!("  {} → {} chunk(s)", doc.id, chunks.len());
    }

    // -- 6. Query the pipeline --------------------------------------------
    let queries = [
        "memory safety without garbage collection",
        "machine learning libraries",
        "embedded database written in Rust",
        "vector database for RAG",
    ];

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
                    &result.chunk.text[..result.chunk.text.len().min(80)],
                );
            }
        }
    }

    // -- 7. Demonstrate upsert (update a document) ------------------------
    println!("\n--- Updating doc1 with new content ---");
    pipeline
        .ingest(
            collection,
            &Document {
                id: "doc1".into(),
                text: "Rust 2024 edition introduced async closures, gen blocks, and \
                       improved pattern matching. The language continues to evolve while \
                       maintaining its zero-cost abstractions and memory safety guarantees."
                    .into(),
                metadata: HashMap::from([("topic".into(), "rust".into())]),
                source_uri: Some("https://www.rust-lang.org".into()),
            },
        )
        .await?;

    let results = pipeline.query(collection, "Rust 2024 new features").await?;
    println!("\nQuery: \"Rust 2024 new features\"");
    for (i, result) in results.iter().enumerate() {
        println!(
            "  {}. [score={:.4}] doc={} | {}",
            i + 1,
            result.score,
            result.chunk.document_id,
            &result.chunk.text[..result.chunk.text.len().min(80)],
        );
    }

    // -- 8. Clean up ------------------------------------------------------
    pipeline.delete_collection(collection).await?;
    println!("\nCleaned up collection. Done.");

    Ok(())
}
