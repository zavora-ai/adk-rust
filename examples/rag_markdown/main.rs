//! # RAG Markdown Example
//!
//! Demonstrates markdown-aware document ingestion using `MarkdownChunker`.
//! Each markdown section becomes a chunk, and the `header_path` metadata
//! preserves the header hierarchy (e.g. "Getting Started > Installation").
//!
//! Uses `InMemoryVectorStore` and `MockEmbeddingProvider` — **zero API keys**.
//!
//! Run: `cargo run --example rag_markdown --features rag`

use std::collections::HashMap;
use std::sync::Arc;

use adk_rag::{
    Document, EmbeddingProvider, InMemoryVectorStore, MarkdownChunker, RagConfig, RagPipeline,
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
// Sample markdown document
// ---------------------------------------------------------------------------

const SAMPLE_MARKDOWN: &str = r#"# ADK-Rust User Guide

## Getting Started

### Installation

Add adk-rust to your Cargo.toml dependencies. The crate supports multiple
feature flags for different LLM providers and backends. By default only the
Gemini provider is enabled.

### Quick Start

Create an agent with a model and tools, then run it through the CLI console.
The agent will respond to user messages and can call tools when needed.

## Agents

### LlmAgent

The LlmAgent is the primary agent type. It wraps an LLM model and a set of
tools. Configure it with a system instruction that guides the model's behavior.

### Workflow Agents

Workflow agents compose multiple sub-agents into pipelines. SequentialAgent
runs agents one after another, ParallelAgent runs them concurrently, and
LoopAgent repeats until a condition is met.

## Tools

### Function Tools

Function tools let you expose Rust functions to the LLM. Annotate a function
with the tool macro and the framework generates the JSON schema automatically.

### MCP Integration

The MCP (Model Context Protocol) integration allows agents to connect to
external tool servers. This enables dynamic tool discovery and cross-language
tool sharing.

## Configuration

### Environment Variables

Set GOOGLE_API_KEY or GEMINI_API_KEY for the Gemini provider. For OpenAI set
OPENAI_API_KEY. Each provider documents its required environment variables.

### Feature Flags

Enable only the providers you need to keep compile times fast. Available flags
include gemini (default), openai, anthropic, deepseek, ollama, and groq.
"#;

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // -- 1. Configure the pipeline ----------------------------------------
    // MarkdownChunker splits by headers, so chunk_size is a maximum for
    // sections that are too long. 500 chars is generous for this document.
    let config = RagConfig::builder()
        .chunk_size(500)
        .chunk_overlap(50)
        .top_k(3)
        .similarity_threshold(0.0)
        .build()?;

    // -- 2. Build the pipeline --------------------------------------------
    // MarkdownChunker preserves header hierarchy in each chunk's metadata
    // under the "header_path" key (e.g. "Getting Started > Installation").
    let pipeline = Arc::new(
        RagPipeline::builder()
            .config(config)
            .embedding_provider(Arc::new(MockEmbeddingProvider::new(64)))
            .vector_store(Arc::new(InMemoryVectorStore::new()))
            .chunker(Arc::new(MarkdownChunker::new(500, 50)))
            .build()?,
    );

    // -- 3. Create collection and ingest the markdown document ------------
    let collection = "docs";
    pipeline.create_collection(collection).await?;

    let doc = Document {
        id: "user_guide".into(),
        text: SAMPLE_MARKDOWN.into(),
        metadata: HashMap::from([("source".into(), "README.md".into())]),
        source_uri: Some("https://github.com/example/adk-rust/README.md".into()),
    };

    let chunks = pipeline.ingest(collection, &doc).await?;
    println!("Ingested {} chunks from markdown document:\n", chunks.len());

    // -- 4. Show how header_path metadata is preserved --------------------
    for (i, chunk) in chunks.iter().enumerate() {
        let header = chunk.metadata.get("header_path").map_or("(none)", |s| s.as_str());
        let preview = &chunk.text[..chunk.text.len().min(60)];
        println!("  Chunk {i}: header_path=\"{header}\"");
        println!("           text=\"{preview}...\"");
    }

    // -- 5. Query by topic ------------------------------------------------
    let queries = ["how to install", "workflow agents", "MCP tool servers"];

    for query in &queries {
        println!("\nQuery: \"{query}\"");
        let results = pipeline.query(collection, query).await?;
        if results.is_empty() {
            println!("  (no results)");
        } else {
            for (i, result) in results.iter().enumerate() {
                let header =
                    result.chunk.metadata.get("header_path").map_or("(none)", |s| s.as_str());
                println!(
                    "  {}. [score={:.4}] section=\"{header}\" | {}",
                    i + 1,
                    result.score,
                    &result.chunk.text[..result.chunk.text.len().min(70)],
                );
            }
        }
    }

    println!("\nDone.");
    Ok(())
}
