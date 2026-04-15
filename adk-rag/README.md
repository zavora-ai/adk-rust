# adk-rag

Give your AI agents a knowledge base. `adk-rag` adds Retrieval-Augmented Generation (RAG) to [ADK-Rust](https://github.com/zavora-ai/adk-rust) so your agents can search documents and answer questions using your own data.

[![Crates.io](https://img.shields.io/crates/v/adk-rag.svg)](https://crates.io/crates/adk-rag)
[![Documentation](https://docs.rs/adk-rag/badge.svg)](https://docs.rs/adk-rag)
[![License](https://img.shields.io/crates/l/adk-rag.svg)](LICENSE)


## ADK RAG
The `adk-rag` crate provides Retrieval-Augmented Generation capabilities for the ADK-Rust workspace. It offers a modular, trait-based architecture for document chunking, embedding generation, vector storage, similarity search, reranking, and agentic retrieval. The crate follows the ADK-Rust conventions of feature-gated backends, async-trait interfaces, and builder-pattern configuration. It integrates with existing ADK crates (`adk-gemini` for embeddings, `adk-core` for the Tool trait) and supports multiple vector store backends (in-memory, Qdrant, LanceDB, pgvector, SurrealDB).

## What is RAG?

RAG stands for Retrieval-Augmented Generation. Instead of relying only on what an LLM was trained on, RAG lets your agent look up relevant information from your documents before answering:

1. **Ingest** — Documents are split into chunks, converted to vector embeddings, and stored
2. **Query** — A user question is embedded and matched against stored chunks
3. **Generate** — The most relevant chunks are passed to the LLM as context

## Quick Start

The fastest way to get a working RAG pipeline. Uses Gemini for embeddings (free API key from [Google AI Studio](https://aistudio.google.com/apikey)).

```toml
[dependencies]
adk-rag = { version = "0.6.0", features = ["gemini"] }
tokio = { version = "1", features = ["full"] }
```

```rust
use std::sync::Arc;
use adk_rag::*;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;

    let pipeline = RagPipeline::builder()
        .config(RagConfig::default())
        .embedding_provider(Arc::new(GeminiEmbeddingProvider::new(&api_key)?))
        .vector_store(Arc::new(InMemoryVectorStore::new()))
        .chunker(Arc::new(RecursiveChunker::new(512, 100)))
        .build()?;

    pipeline.create_collection("docs").await?;

    pipeline.ingest("docs", &Document {
        id: "intro".into(),
        text: "Rust is a systems programming language focused on safety and speed.".into(),
        metadata: Default::default(),
        source_uri: None,
    }).await?;

    let results = pipeline.query("docs", "safe programming language").await?;
    for r in &results {
        println!("[score: {:.3}] {}", r.score, r.chunk.text);
    }
    Ok(())
}
```

```bash
GOOGLE_API_KEY=your-key-here cargo run
```

## Agent with RAG Tool

The practical use case — an agent that searches your knowledge base to answer questions. The agent decides when to call `rag_search` and uses the retrieved context to generate answers.

```toml
[dependencies]
adk-rag = { version = "0.6.0", features = ["gemini"] }
adk-agent = "0.6.0"
adk-model = "0.6.0"
adk-cli = "0.6.0"
tokio = { version = "1", features = ["full"] }
```

```rust
use std::sync::Arc;
use adk_agent::LlmAgentBuilder;
use adk_model::GeminiModel;
use adk_rag::*;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;

    // Build the RAG pipeline
    let pipeline = Arc::new(
        RagPipeline::builder()
            .config(RagConfig::builder().chunk_size(300).chunk_overlap(50).top_k(3).build()?)
            .embedding_provider(Arc::new(GeminiEmbeddingProvider::new(&api_key)?))
            .vector_store(Arc::new(InMemoryVectorStore::new()))
            .chunker(Arc::new(RecursiveChunker::new(300, 50)))
            .build()?,
    );

    // Ingest your documents
    pipeline.create_collection("kb").await?;
    pipeline.ingest("kb", &Document {
        id: "faq".into(),
        text: "Our return policy allows returns within 30 days with a receipt.".into(),
        metadata: Default::default(),
        source_uri: None,
    }).await?;

    // Create an agent with RAG as a tool
    let agent = LlmAgentBuilder::new("support_agent")
        .instruction("Answer questions using the rag_search tool. Cite your sources.")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?))
        .tool(Arc::new(RagTool::new(pipeline, "kb")))
        .build()?;

    // Interactive chat
    adk_cli::console::run_console(Arc::new(agent), "app".into(), "user1".into()).await?;
    Ok(())
}
```

The agent will automatically call `rag_search` when it needs information from your knowledge base.

## How It Works

`adk-rag` is built from four pluggable components:

```
Documents → [Chunker] → [EmbeddingProvider] → [VectorStore]
                                                     ↓
Query → [EmbeddingProvider] → [VectorStore search] → [Reranker] → Results
```

| Component | What it does | Built-in options |
|-----------|-------------|-----------------|
| **Chunker** | Splits documents into smaller pieces | `FixedSizeChunker`, `RecursiveChunker`, `MarkdownChunker` |
| **EmbeddingProvider** | Converts text to vector embeddings | `GeminiEmbeddingProvider`¹, `OpenAIEmbeddingProvider`² |
| **VectorStore** | Stores and searches embeddings | `InMemoryVectorStore`, `QdrantVectorStore`³, `LanceDBVectorStore`⁴, `PgVectorStore`⁵, `SurrealVectorStore`⁶ |
| **Reranker** | Re-scores results after search | `NoOpReranker` (default), or write your own |

¹ `gemini` feature  ² `openai` feature  ³ `qdrant` feature  ⁴ `lancedb` feature  ⁵ `pgvector` feature  ⁶ `surrealdb` feature

The `RagPipeline` wires these together. The `RagTool` wraps the pipeline as an `adk_core::Tool` so any ADK agent can call it.

## Embedding Providers

### Gemini (recommended)

Uses Google's `gemini-embedding-001` model (3072 dimensions). Free tier available.

```toml
adk-rag = { version = "0.6.0", features = ["gemini"] }
```

```rust
let provider = GeminiEmbeddingProvider::new(&api_key)?;
```

### OpenAI

Uses `text-embedding-3-small` (1536 dimensions) by default. Supports dimension truncation via Matryoshka.

```toml
adk-rag = { version = "0.6.0", features = ["openai"] }
```

```rust
// Default model
let provider = OpenAIEmbeddingProvider::new("sk-...")?;

// Or read from OPENAI_API_KEY env var
let provider = OpenAIEmbeddingProvider::from_env()?;

// With a different model and custom dimensions
let provider = OpenAIEmbeddingProvider::new("sk-...")?
    .with_model("text-embedding-3-large")
    .with_dimensions(256);
```

### Custom Embedding Provider

Implement the `EmbeddingProvider` trait to use any embedding model — a local model, a different API, or a mock for testing.

```rust
use async_trait::async_trait;
use adk_rag::{EmbeddingProvider, Result};

struct MyEmbedder { /* your client */ }

#[async_trait]
impl EmbeddingProvider for MyEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        // Call your embedding model here
        todo!()
    }

    fn dimensions(&self) -> usize {
        384 // Return your model's output dimensions
    }
}
```

Add `async-trait = "0.1"` to your `Cargo.toml` when implementing traits.

## Vector Stores

### InMemoryVectorStore (default)

No external dependencies. Good for development, testing, and small datasets. Data is lost when the process exits.

```rust
let store = InMemoryVectorStore::new();
```

### Qdrant

Production-ready vector database with filtering, snapshots, and clustering.

```toml
adk-rag = { version = "0.6.0", features = ["qdrant"] }
```

```rust
let store = QdrantVectorStore::new("http://localhost:6334").await?;
```

### LanceDB

Embedded vector database with no server required. Data persists to disk.

```toml
adk-rag = { version = "0.6.0", features = ["lancedb"] }
```

> Requires `protoc` installed: `brew install protobuf` (macOS), `apt install protobuf-compiler` (Ubuntu).

```rust
let store = LanceDBVectorStore::new("/tmp/my-vectors").await?;
```

### pgvector (PostgreSQL)

Use your existing PostgreSQL database for vector search.

```toml
adk-rag = { version = "0.6.0", features = ["pgvector"] }
```

```rust
let store = PgVectorStore::new("postgres://user:pass@localhost/mydb").await?;
```

### SurrealDB

Embedded or remote multi-model database with built-in vector search.

```toml
adk-rag = { version = "0.6.0", features = ["surrealdb"] }
```

```rust
let store = SurrealVectorStore::new_memory().await?;
// or
let store = SurrealVectorStore::new_rocksdb("/tmp/surreal-data").await?;
```

## Choosing a Chunker

| Chunker | Best for | How it splits |
|---------|----------|--------------|
| `FixedSizeChunker` | General text, logs | Every N characters with overlap |
| `RecursiveChunker` | Articles, docs, code | Paragraphs → sentences → words (natural boundaries) |
| `MarkdownChunker` | Markdown files, READMEs | By headers, preserving section hierarchy in metadata |

```rust
// Fixed: 512 chars per chunk, 100 char overlap
let chunker = FixedSizeChunker::new(512, 100);

// Recursive: tries paragraph breaks first, then sentences
let chunker = RecursiveChunker::new(512, 100);

// Markdown: splits by headers, stores header path in metadata
let chunker = MarkdownChunker::new(512, 100);
```

## Configuration

```rust
let config = RagConfig::builder()
    .chunk_size(256)            // max characters per chunk (default: 512)
    .chunk_overlap(50)          // overlap between chunks (default: 100)
    .top_k(5)                   // number of results to return (default: 10)
    .similarity_threshold(0.5)  // minimum score to include (default: 0.0)
    .build()?;
```

- **chunk_size** — Smaller chunks are more precise but may lose context. 200–500 is a good range.
- **chunk_overlap** — Prevents information loss at chunk boundaries. 10–20% of chunk_size works well.
- **top_k** — More results give the LLM more context but increase token usage.
- **similarity_threshold** — Filter out low-quality matches. 0.0 returns everything, 0.3–0.7 keeps strong matches only.

## Writing a Custom Reranker

The default `NoOpReranker` passes results through unchanged. Write your own to improve precision:

```toml
[dependencies]
adk-rag = { version = "0.6.0", features = ["gemini"] }
async-trait = "0.1"
tokio = { version = "1", features = ["full"] }
```

```rust
use async_trait::async_trait;
use adk_rag::{Reranker, SearchResult};

struct KeywordBoostReranker {
    boost: f32,
}

#[async_trait]
impl Reranker for KeywordBoostReranker {
    async fn rerank(
        &self,
        query: &str,
        mut results: Vec<SearchResult>,
    ) -> adk_rag::Result<Vec<SearchResult>> {
        let keywords: Vec<String> = query.split_whitespace()
            .filter(|w| w.len() > 3)
            .map(|w| w.to_lowercase())
            .collect();

        for result in &mut results {
            let text = result.chunk.text.to_lowercase();
            let hits = keywords.iter().filter(|kw| text.contains(kw.as_str())).count();
            result.score += hits as f32 * self.boost;
        }

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        Ok(results)
    }
}
```

Use it in the pipeline:

```rust
let pipeline = RagPipeline::builder()
    .config(config)
    .embedding_provider(embedder)
    .vector_store(store)
    .chunker(chunker)
    .reranker(Arc::new(KeywordBoostReranker { boost: 0.1 }))
    .build()?;
```

## Feature Flags

```toml
# Core only (in-memory store, all chunkers, no external deps)
adk-rag = "0.6.0"

# With Gemini embeddings (recommended)
adk-rag = { version = "0.6.0", features = ["gemini"] }

# With OpenAI embeddings
adk-rag = { version = "0.6.0", features = ["openai"] }

# With a persistent vector store
adk-rag = { version = "0.6.0", features = ["gemini", "qdrant"] }

# Everything
adk-rag = { version = "0.6.0", features = ["full"] }
```

| Feature | Enables | Extra dependency |
|---------|---------|-----------------|
| *(default)* | Core traits, `InMemoryVectorStore`, all chunkers | none |
| `gemini` | `GeminiEmbeddingProvider` | `adk-gemini` |
| `openai` | `OpenAIEmbeddingProvider` | `reqwest` |
| `qdrant` | `QdrantVectorStore` | `qdrant-client` |
| `lancedb` | `LanceDBVectorStore` | `lancedb`, `arrow` |
| `pgvector` | `PgVectorStore` | `sqlx` |
| `surrealdb` | `SurrealVectorStore` | `surrealdb` |
| `full` | All of the above | all |

## Testing Without API Keys

For unit tests or CI where you don't have API keys, implement a deterministic mock embedder:

```rust
use async_trait::async_trait;
use adk_rag::{EmbeddingProvider, Result};

struct MockEmbedder;

#[async_trait]
impl EmbeddingProvider for MockEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        // Deterministic hash-based embedding for testing.
        // NOT suitable for production — use a real provider.
        let hash = text.bytes().fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
        let mut v = vec![0.0f32; 64];
        for (i, x) in v.iter_mut().enumerate() {
            *x = ((hash.wrapping_add(i as u64)) as f32).sin();
        }
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 { v.iter_mut().for_each(|x| *x /= norm); }
        Ok(v)
    }
    fn dimensions(&self) -> usize { 64 }
}
```

This produces stable vectors so your tests are reproducible, but the similarity scores won't be meaningful. Use it for testing pipeline wiring, not search quality.

## Examples

Run from the ADK-Rust workspace root:

| Example | What it shows | API key? | Command |
|---------|--------------|----------|---------|
| `rag_basic` | Pipeline with mock embeddings | No | `cargo run --example rag_basic --features rag` |
| `rag_markdown` | Markdown chunking with header metadata | No | `cargo run --example rag_markdown --features rag` |
| `rag_agent` | LlmAgent with RagTool | Yes | `cargo run --example rag_agent --features rag-gemini` |
| `rag_recursive` | Codebase Q&A with RecursiveChunker | Yes | `cargo run --example rag_recursive --features rag-gemini` |
| `rag_reranker` | Custom keyword reranker | Yes | `cargo run --example rag_reranker --features rag-gemini` |
| `rag_multi_collection` | Multi-collection search | Yes | `cargo run --example rag_multi_collection --features rag-gemini` |
| `rag_surrealdb` | SurrealDB vector store | Yes | `cargo run --example rag_surrealdb --features rag-surrealdb` |

For examples that need an API key, set `GOOGLE_API_KEY` in your environment or `.env` file.

## API Reference

### Core Types

```rust
// A document to ingest
Document {
    id: String,
    text: String,
    metadata: HashMap<String, String>,
    source_uri: Option<String>,
}

// A chunk produced by a Chunker (with embedding attached after processing)
Chunk {
    id: String,
    text: String,
    embedding: Vec<f32>,
    metadata: HashMap<String, String>,
    document_id: String,
}

// A search result with relevance score
SearchResult {
    chunk: Chunk,
    score: f32,
}
```

### Pipeline Methods

```rust
let pipeline = RagPipeline::builder()
    .config(config)
    .embedding_provider(embedder)
    .vector_store(store)
    .chunker(chunker)
    .reranker(reranker)  // optional
    .build()?;

// Collection management
pipeline.create_collection("name").await?;
pipeline.delete_collection("name").await?;

// Ingestion (chunk → embed → store)
let chunks = pipeline.ingest("collection", &document).await?;
let chunks = pipeline.ingest_batch("collection", &documents).await?;

// Query (embed → search → rerank → filter)
let results = pipeline.query("collection", "search text").await?;
```

### RagTool

Wraps a pipeline as an `adk_core::Tool` for agent use:

```rust
let tool = RagTool::new(pipeline, "default_collection");

// The agent calls it with JSON:
// { "query": "How do I reset my password?" }
// { "query": "pricing info", "collection": "faq", "top_k": 5 }
```

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.
