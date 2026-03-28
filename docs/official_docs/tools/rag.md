# RAG (Retrieval-Augmented Generation)

Give your agents a knowledge base so they can answer questions using your own data.

---

## What is RAG?

RAG lets your agent look up relevant information from your documents before answering a question. Instead of relying only on what the LLM was trained on, the agent searches your data and uses the results as context.

The flow is:

1. **Ingest** — Documents are split into chunks, converted to vector embeddings, and stored
2. **Query** — A question is embedded and matched against stored chunks by similarity
3. **Generate** — The most relevant chunks are passed to the LLM as context for its answer

This means your agent can answer questions about product docs, company policies, codebases, or any text you feed it.

> **Key highlights**:
> - 📄 **Ingest any text** — product docs, markdown, code, policies
> - 🔍 **Semantic search** — find relevant content by meaning, not just keywords
> - 🤖 **Agentic retrieval** — the agent decides when to search via `RagTool`
> - 🔌 **Pluggable backends** — swap embedding providers and vector stores without changing code

---

## Installation

```toml
[dependencies]
# Core only (in-memory store, all chunkers, no external deps)
adk-rag = "0.5.0"

# With Gemini embeddings (recommended for getting started)
adk-rag = { version = "0.5.0", features = ["gemini"] }
```

---

## Step 1: Build a Pipeline

A `RagPipeline` wires together four components: a chunker, an embedding provider, a vector store, and an optional reranker.

```rust
use std::collections::HashMap;
use std::sync::Arc;
use adk_rag::{
    Document, FixedSizeChunker, InMemoryVectorStore,
    RagConfig, RagPipeline, EmbeddingProvider,
};

// Mock embedder for demos — no API key needed.
// In production, use GeminiEmbeddingProvider or OpenAIEmbeddingProvider.
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
        if norm > 0.0 { v.iter_mut().for_each(|x| *x /= norm); }
        Ok(v)
    }
    fn dimensions(&self) -> usize { 64 }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pipeline = RagPipeline::builder()
        .config(RagConfig::builder()
            .chunk_size(256)
            .chunk_overlap(50)
            .top_k(3)
            .build()?)
        .embedding_provider(Arc::new(MockEmbedder))
        .vector_store(Arc::new(InMemoryVectorStore::new()))
        .chunker(Arc::new(FixedSizeChunker::new(256, 50)))
        .build()?;

    // Create a collection and ingest a document
    pipeline.create_collection("docs").await?;
    pipeline.ingest("docs", &Document {
        id: "intro".into(),
        text: "Rust is a systems programming language focused on safety and speed.".into(),
        metadata: HashMap::from([("topic".into(), "rust".into())]),
        source_uri: None,
    }).await?;

    // Query
    let results = pipeline.query("docs", "safe programming").await?;
    for r in &results {
        println!("[{:.3}] {}", r.score, r.chunk.text);
    }
    Ok(())
}
```

**How it works**:
1. `FixedSizeChunker` splits the document into 256-character chunks with 50-character overlap
2. `MockEmbedder` converts each chunk into a 64-dimensional vector
3. `InMemoryVectorStore` stores the vectors and searches by cosine similarity
4. `query()` embeds the question, finds the closest chunks, and returns them ranked by score

---

## Step 2: Add RAG to an Agent

The real power of RAG is when an agent uses it as a tool. `RagTool` wraps the pipeline as an `adk_core::Tool` — the agent calls `rag_search` whenever it needs information.

When you use `RagTool` with Gemini-backed agents, ADK automatically normalizes the tool result into a Gemini-compatible function response. This matters because `rag_search` naturally returns a list of chunks, while Gemini expects `functionResponse.response` to be a JSON object on the wire.

```rust
use std::sync::Arc;
use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use adk_rag::{
    Document, GeminiEmbeddingProvider, InMemoryVectorStore,
    RagConfig, RagPipeline, RagTool, RecursiveChunker,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;

    // Build pipeline with real embeddings
    let pipeline = Arc::new(
        RagPipeline::builder()
            .config(RagConfig::builder().chunk_size(300).chunk_overlap(50).top_k(3).build()?)
            .embedding_provider(Arc::new(GeminiEmbeddingProvider::new(&api_key)?))
            .vector_store(Arc::new(InMemoryVectorStore::new()))
            .chunker(Arc::new(RecursiveChunker::new(300, 50)))
            .build()?,
    );

    // Ingest documents
    pipeline.create_collection("kb").await?;
    pipeline.ingest("kb", &Document {
        id: "returns".into(),
        text: "Our return policy allows returns within 30 days with a receipt.".into(),
        metadata: Default::default(),
        source_uri: None,
    }).await?;

    // Wrap pipeline as a tool and attach to an agent
    let agent = LlmAgentBuilder::new("support")
        .instruction("Answer questions using the rag_search tool. Cite your sources.")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?))
        .tool(Arc::new(RagTool::new(pipeline, "kb")))
        .build()?;

    // The agent now calls rag_search automatically when it needs knowledge base info
    adk_cli::console::run_console(Arc::new(agent), "app".into(), "user1".into()).await?;
    Ok(())
}
```

When a user asks "What's your return policy?", the agent:
1. Decides it needs to search the knowledge base
2. Calls `rag_search` with `{"query": "return policy"}`
3. Gets back the relevant chunks with scores
4. Uses the chunks as context to generate a natural answer

---

## Step 3: Choose a Chunking Strategy

How you split documents affects retrieval quality. `adk-rag` provides three chunkers:

| Chunker | Best for | How it splits |
|---------|----------|--------------|
| `FixedSizeChunker` | General text, logs | Every N characters with overlap |
| `RecursiveChunker` | Articles, docs, code comments | Paragraphs → sentences → words |
| `MarkdownChunker` | Markdown files, READMEs | By headers, preserving section hierarchy |

```rust
use adk_rag::{FixedSizeChunker, RecursiveChunker, MarkdownChunker};

// Fixed: simple, predictable chunks
let chunker = FixedSizeChunker::new(512, 100);

// Recursive: respects natural text boundaries
let chunker = RecursiveChunker::new(512, 100);

// Markdown: each section becomes a chunk with header_path metadata
let chunker = MarkdownChunker::new(512, 100);
```

`RecursiveChunker` is the best default choice — it tries paragraph breaks first, then sentence boundaries, then word boundaries, producing more natural chunks than fixed-size splitting.

`MarkdownChunker` adds a `header_path` metadata field to each chunk (e.g. `"Getting Started > Installation"`), which helps the agent cite specific sections.

---

## Configuration

```rust
use adk_rag::RagConfig;

let config = RagConfig::builder()
    .chunk_size(256)            // max characters per chunk (default: 512)
    .chunk_overlap(50)          // overlap between chunks (default: 100)
    .top_k(5)                   // results to return (default: 10)
    .similarity_threshold(0.5)  // minimum score to include (default: 0.0)
    .build()?;
```

| Parameter | What it controls | Guidance |
|-----------|-----------------|----------|
| `chunk_size` | Max characters per chunk | 200–500 for most use cases. Smaller = more precise, larger = more context |
| `chunk_overlap` | Shared characters between adjacent chunks | 10–20% of chunk_size prevents losing info at boundaries |
| `top_k` | Number of results returned | More results = more context for the LLM but higher token usage |
| `similarity_threshold` | Minimum score to include | 0.0 returns everything; 0.3–0.7 filters weak matches |

---

## Embedding Providers

| Provider | Feature flag | Model | Requires |
|----------|-------------|-------|----------|
| `GeminiEmbeddingProvider` | `gemini` | gemini-embedding-001 | `GOOGLE_API_KEY` |
| `OpenAIEmbeddingProvider` | `openai` | text-embedding-3-small | `OPENAI_API_KEY` |

```rust
// Gemini
use adk_rag::GeminiEmbeddingProvider;
let embedder = GeminiEmbeddingProvider::new(&api_key)?;

// OpenAI
use adk_rag::OpenAIEmbeddingProvider;
let embedder = OpenAIEmbeddingProvider::new(&api_key, "text-embedding-3-small");
```

You can also implement `EmbeddingProvider` for any custom embedding service.

---

## Vector Store Backends

| Backend | Feature flag | Best for |
|---------|-------------|----------|
| `InMemoryVectorStore` | *(default)* | Development, testing, small datasets |
| `QdrantVectorStore` | `qdrant` | Production with dedicated vector DB |
| `LanceDBVectorStore` | `lancedb` | Embedded vector DB (no server needed) |
| `PgVectorStore` | `pgvector` | When you already use PostgreSQL |

```rust
// In-memory (no setup needed)
use adk_rag::InMemoryVectorStore;
let store = InMemoryVectorStore::new();

// Qdrant (requires running Qdrant server)
use adk_rag::QdrantVectorStore;
let store = QdrantVectorStore::new("http://localhost:6334").await?;

// pgvector (requires PostgreSQL with pgvector extension)
use adk_rag::PgVectorStore;
let store = PgVectorStore::new("postgres://user:pass@localhost/db").await?;
```

---

## Custom Reranker

The default `NoOpReranker` passes results through unchanged. Write a custom reranker to improve precision:

```rust
use adk_rag::{Reranker, SearchResult};

struct KeywordBoostReranker { boost: f32 }

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

        for r in &mut results {
            let text = r.chunk.text.to_lowercase();
            let hits = keywords.iter().filter(|kw| text.contains(kw.as_str())).count();
            r.score += hits as f32 * self.boost;
        }
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        Ok(results)
    }
}

// Add to pipeline
let pipeline = RagPipeline::builder()
    .config(config)
    .embedding_provider(embedder)
    .vector_store(store)
    .chunker(chunker)
    .reranker(Arc::new(KeywordBoostReranker { boost: 0.1 }))
    .build()?;
```

---

## Multiple Collections

Use separate collections for different knowledge domains. The agent can search specific collections or you can create multiple `RagTool` instances:

```rust
// Create collections for different content types
pipeline.create_collection("docs").await?;
pipeline.create_collection("faq").await?;
pipeline.create_collection("changelog").await?;

// Ingest into each
pipeline.ingest("docs", &setup_doc).await?;
pipeline.ingest("faq", &faq_doc).await?;
pipeline.ingest("changelog", &release_doc).await?;

// One tool per collection — the agent picks which to search
let docs_tool = RagTool::new(pipeline.clone(), "docs");
let faq_tool = RagTool::new(pipeline.clone(), "faq");

let agent = LlmAgentBuilder::new("support")
    .instruction("Search 'docs' for how-to questions, 'faq' for common questions.")
    .model(Arc::new(model))
    .tool(Arc::new(docs_tool))
    .tool(Arc::new(faq_tool))
    .build()?;
```

The agent can also override the collection at query time by passing `"collection": "faq"` in the tool call.

---

## Feature Flags

Only pull the dependencies you need:

| Feature | Enables | Extra dependency |
|---------|---------|-----------------|
| *(default)* | Core traits, `InMemoryVectorStore`, all chunkers | none |
| `gemini` | `GeminiEmbeddingProvider` | `adk-gemini` |
| `openai` | `OpenAIEmbeddingProvider` | `reqwest` |
| `qdrant` | `QdrantVectorStore` | `qdrant-client` |
| `lancedb` | `LanceDBVectorStore` | `lancedb`, `arrow` |
| `pgvector` | `PgVectorStore` | `sqlx` |
| `full` | All of the above | all |

```toml
# Just core
adk-rag = "0.5.0"

# With Gemini embeddings
adk-rag = { version = "0.5.0", features = ["gemini"] }

# Everything
adk-rag = { version = "0.5.0", features = ["full"] }
```

> **Note:** The `lancedb` feature requires `protoc` installed. Install with `brew install protobuf` (macOS) or `apt install protobuf-compiler` (Ubuntu).

---

## Architecture

```
                        Ingestion
Documents ──→ [Chunker] ──→ [EmbeddingProvider] ──→ [VectorStore]

                          Query
Question ──→ [EmbeddingProvider] ──→ [VectorStore search] ──→ [Reranker] ──→ Results
                                                                               │
                                                                               ▼
                                                                    Agent uses as context
```

The `RagPipeline` orchestrates both flows. The `RagTool` wraps the pipeline as an `adk_core::Tool` so agents call it on demand.

---

## Run Examples

```bash
# No API key needed
cargo run --example rag_basic --features rag
cargo run --example rag_markdown --features rag

# Requires GOOGLE_API_KEY
cargo run --example rag_agent --features rag-gemini
cargo run --example rag_recursive --features rag-gemini
cargo run --example rag_reranker --features rag-gemini
cargo run --example rag_multi_collection --features rag-gemini
```

---

## Best Practices

| Practice | Why |
|----------|-----|
| Use `RecursiveChunker` as default | Produces natural chunk boundaries |
| Keep chunks 200–500 chars | Balances precision and context |
| Use real embeddings in production | Mock embedders are for testing only |
| Set `similarity_threshold` > 0 | Filters out irrelevant noise |
| Separate collections by domain | Improves precision and lets agents target searches |
| Use `InMemoryVectorStore` for dev only | Switch to Qdrant/pgvector for production |

---

## Related

- [Function Tools](function-tools.md) - Creating custom tools
- [LlmAgent](../agents/llm-agent.md) - Adding tools to agents
- [Memory](../security/memory.md) - Long-term memory (different from RAG)

---

**Previous**: [← UI Tools](ui-tools.md) | **Next**: [Sessions →](../sessions/sessions.md)
