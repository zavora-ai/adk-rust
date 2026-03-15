//! RAG + Gemini Thinking Model Example
//!
//! Demonstrates retrieval-augmented generation with a Gemini thinking model.
//! This exercises the exact scenario from issue #205: multi-turn tool calling
//! with a thinking model that produces `thoughtSignature` in responses.
//!
//! ```bash
//! GOOGLE_API_KEY=... cargo run --example rag_gemini --features rag-gemini
//! ```

use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use adk_rag::{
    Document, FixedSizeChunker, GeminiEmbeddingProvider, InMemoryVectorStore, RagConfig,
    RagPipeline, RagTool,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    // Use the latest thinking model — this is the scenario that was crashing in #205
    let model_name =
        std::env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".to_string());

    println!("=== RAG + Gemini Thinking Model ===");
    println!("Model: {model_name}");
    println!();

    // --- Build RAG pipeline with Gemini embeddings + in-memory store ---
    let embedding_provider = GeminiEmbeddingProvider::new(&api_key)?;
    let vector_store = InMemoryVectorStore::new();
    let chunker = FixedSizeChunker::new(256, 50);

    let pipeline = Arc::new(
        RagPipeline::builder()
            .config(RagConfig { top_k: 3, similarity_threshold: 0.0, ..Default::default() })
            .embedding_provider(Arc::new(embedding_provider))
            .vector_store(Arc::new(vector_store))
            .chunker(Arc::new(chunker))
            .build()?,
    );

    // --- Ingest sample documents ---
    pipeline.create_collection("docs").await?;

    let documents = vec![
        Document {
            id: "rust-ownership".into(),
            text: "Rust's ownership system ensures memory safety without garbage collection. \
                   Each value has exactly one owner at a time. When the owner goes out of scope, \
                   the value is dropped. Ownership can be transferred (moved) or borrowed via references."
                .into(),
            metadata: Default::default(),
            source_uri: None,
        },
        Document {
            id: "rust-async".into(),
            text: "Async Rust uses futures and the async/await syntax for concurrent programming. \
                   Futures are lazy — they do nothing until polled. The tokio runtime is the most \
                   popular executor. Use async-trait for async methods in trait definitions."
                .into(),
            metadata: Default::default(),
            source_uri: None,
        },
        Document {
            id: "rust-error-handling".into(),
            text: "Rust uses Result<T, E> for recoverable errors and panic! for unrecoverable ones. \
                   The thiserror crate provides derive macros for custom error types. The anyhow crate \
                   is popular for application-level error handling with context."
                .into(),
            metadata: Default::default(),
            source_uri: None,
        },
    ];

    for doc in &documents {
        pipeline.ingest("docs", doc).await?;
    }
    println!("Ingested {} documents into 'docs' collection", documents.len());

    // --- Build agent with RAG tool + thinking model ---
    let model = GeminiModel::new(&api_key, &model_name)?;
    let rag_tool = RagTool::new(pipeline, "docs");

    let agent = LlmAgentBuilder::new("rag_agent")
        .description("Knowledge assistant with RAG retrieval")
        .instruction(
            "You are a Rust programming assistant. Use the rag_search tool to find \
             relevant information before answering. Always cite which document you \
             found the information in. Be concise.",
        )
        .model(Arc::new(model))
        .tool(Arc::new(rag_tool))
        .build()?;

    println!("Agent ready. Try: 'How does Rust handle memory safety?'\n");

    adk_cli::console::run_console(
        Arc::new(agent),
        "rag_gemini_app".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
