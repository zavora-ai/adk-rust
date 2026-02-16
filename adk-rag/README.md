# adk-rag

Give your AI agents a knowledge base. `adk-rag` adds Retrieval-Augmented Generation (RAG) to [ADK-Rust](https://github.com/zavora-ai/adk-rust) so your agents can search documents and answer questions using your own data.

[![Crates.io](https://img.shields.io/crates/v/adk-rag.svg)](https://crates.io/crates/adk-rag)
[![Documentation](https://docs.rs/adk-rag/badge.svg)](https://docs.rs/adk-rag)
[![License](https://img.shields.io/crates/l/adk-rag.svg)](LICENSE)

## ADK RAG
The `adk-rag` crate provides Retrieval-Augmented Generation capabilities for the ADK-Rust workspace. It offers a modular, trait-based architecture for document chunking, embedding generation, vector storage, similarity search, reranking, and agentic retrieval. The crate follows the ADK-Rust conventions of feature-gated backends, async-trait interfaces, and builder-pattern configuration. It integrates with existing ADK crates (`adk-gemini` for embeddings, `adk-core` for the Tool trait) and supports multiple vector store backends (in-memory, Qdrant, LanceDB, pgvector, SurrealDB).

## What is RAG?

RAG stands for Retrieval-Augmented Generation. Instead of relying only on what an LLM was trained on, RAG lets your agent look up relevant information from your documents before answering. The flow is:

1. **Ingest** — Your documents are split into chunks, converted to vector embeddings, and stored
2. **Query** — When a user asks a question, the question is embedded and matched against stored chunks
3. **Generate** — The most relevant chunks are passed to the LLM as context for its answer

This means your agent can answer questions about your product docs, company policies, codebases, or any text you feed it.

## Quick Start

Add `adk-rag` to your `Cargo.toml`:

```toml
[dependencies]
adk-rag = "0.3"
```

### Minimal example — no API keys needed

This uses the built-in `InMemoryVectorStore` and a simple mock embedder. Good for trying things out locally.

```rust
use std::sync::Arc;
use adk_rag::*;

// A mock embedder that turns text into vectors using hashing.
// In production, swap this for GeminiEmbeddingProvider or OpenAIEmbeddingProvider.
struct MockEmbedder;

#[async_trait::async_trait]
impl EmbeddingProvider for MockEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
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

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // 1. Build a pipeline
    let pipeline = RagPipeline::builder()
        .config(RagConfig::default())
        .embedding_provider(Arc::new(MockEmbedder))
        .vector_store(Arc::new(InMemoryVectorStore::new()))
        .chunker(Arc::new(FixedSizeChunker::new(512, 100)))
        .build()?;

    // 2. Create a collection and add a document
    pipeline.create_collection("docs").await?;
    pipeline.ingest("docs", &Document {
        id: "intro".into(),
        text: "Rust is a systems programming language focused on safety and speed.".into(),
        metadata: Default::default(),
        source_uri: None,
    }).await?;

    // 3. Search
    let results = pipeline.query("docs", "safe programming language").await?;
    for r in &results {
        println!("[score: {:.3}] {}", r.score, r.chunk.text);
    }
    Ok(())
}
```

### With a real LLM agent

This is the practical use case — an agent that searches your knowledge base to answer questions. Requires a `GOOGLE_API_KEY`.

```toml
[dependencies]
adk-rag = { version = "0.3", features = ["gemini"] }
adk-agent = "0.3"
adk-model = "0.3"
adk-cli = "0.3"
```

```rust
use std::sync::Arc;
use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use adk_rag::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;

    // Build the RAG pipeline with real embeddings
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
| **Reranker** | Re-scores results after search | `NoOpReranker` (or write your own) |

¹ requires `gemini` feature  ² requires `openai` feature  ³ requires `qdrant` feature  ⁴ requires `lancedb` feature  ⁵ requires `pgvector` feature  ⁶ requires `surrealdb` feature

The `RagPipeline` wires these together. The `RagTool` wraps the pipeline as an `adk_core::Tool` so any ADK agent can call it.

## Choosing a Chunker

| Chunker | Best for | How it splits |
|---------|----------|--------------|
| `FixedSizeChunker` | General text, logs | Every N characters with overlap |
| `RecursiveChunker` | Articles, docs, code comments | Paragraphs → sentences → words (natural boundaries) |
| `MarkdownChunker` | Markdown files, READMEs | By headers, preserving section hierarchy in metadata |

```rust
// Fixed: 512 chars per chunk, 100 char overlap
let chunker = FixedSizeChunker::new(512, 100);

// Recursive: tries paragraph breaks first, then sentences
let chunker = RecursiveChunker::new(512, 100);

// Markdown: splits by ## headers, stores header path in metadata
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

- **chunk_size** — Smaller chunks are more precise but may lose context. Larger chunks preserve context but may include irrelevant text. 200–500 is a good range for most use cases.
- **chunk_overlap** — Overlap ensures important information at chunk boundaries isn't lost. 10–20% of chunk_size works well.
- **top_k** — How many results to return. More results give the LLM more context but increase token usage.
- **similarity_threshold** — Filter out low-quality matches. Set to 0.0 to return everything, or 0.3–0.7 to only keep strong matches.

## Feature Flags

Only pull the dependencies you need:

```toml
# Just the core (in-memory store, all chunkers, no external deps)
adk-rag = "0.3"

# With Gemini embeddings
adk-rag = { version = "0.3", features = ["gemini"] }

# With OpenAI embeddings
adk-rag = { version = "0.3", features = ["openai"] }

# With Qdrant vector store
adk-rag = { version = "0.3", features = ["qdrant"] }

# With SurrealDB vector store (embedded or remote)
adk-rag = { version = "0.3", features = ["surrealdb"] }

# Everything
adk-rag = { version = "0.3", features = ["full"] }
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

> **Note:** The `lancedb` feature requires `protoc` (Protocol Buffers compiler) installed on your system. Install with `brew install protobuf` (macOS), `apt install protobuf-compiler` (Ubuntu), or `choco install protoc` (Windows).

## Writing a Custom Reranker

The default `NoOpReranker` passes results through unchanged. You can write your own to improve precision:

```rust
use adk_rag::{Reranker, SearchResult};

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

// Use it in the pipeline
let pipeline = RagPipeline::builder()
    .config(config)
    .embedding_provider(embedder)
    .vector_store(store)
    .chunker(chunker)
    .reranker(Arc::new(KeywordBoostReranker { boost: 0.1 }))
    .build()?;
```

## Examples

Run these from the ADK-Rust workspace root:

| Example | What it shows | API key needed? | Command |
|---------|--------------|----------------|---------|
| `rag_basic` | Pipeline fundamentals with mock embeddings | No | `cargo run --example rag_basic --features rag` |
| `rag_markdown` | Markdown-aware chunking with header metadata | No | `cargo run --example rag_markdown --features rag` |
| `rag_agent` | LlmAgent with RagTool for product support | Yes | `cargo run --example rag_agent --features rag-gemini` |
| `rag_recursive` | Codebase Q&A agent with RecursiveChunker | Yes | `cargo run --example rag_recursive --features rag-gemini` |
| `rag_reranker` | HR policy agent with custom keyword reranker | Yes | `cargo run --example rag_reranker --features rag-gemini` |
| `rag_multi_collection` | Support agent searching docs, troubleshooting, and changelog collections | Yes | `cargo run --example rag_multi_collection --features rag-gemini` |

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
