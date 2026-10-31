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

| Capability | How |
|------------|-----|
| Ingest any text | Product docs, markdown, code, policies, split by a chunker |
| Semantic search | Matches by embedding similarity, not keywords |
| Agentic retrieval | The agent decides when to search through `RagTool` |
| Pluggable backends | Embedding providers and vector stores are traits |

---

## Installation

```toml
[dependencies]
# Core only (in-memory store, all chunkers, no external deps)
adk-rag = "3.0.0"

# With Gemini embeddings (recommended for getting started)
adk-rag = { version = "3.0.0", features = ["gemini"] }
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
1. `FixedSizeChunker` splits the document into 256-byte chunks with 50-byte overlap — the
   chunker's own sizes decide chunking
2. `MockEmbedder` converts each chunk into a 64-dimensional vector
3. `InMemoryVectorStore` stores the vectors and searches by cosine similarity
4. `query()` embeds the question, finds the closest chunks, and returns them ranked by score;
   `query_with_top_k()` does the same with a result count of its own

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
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?))
        .tool(Arc::new(RagTool::new(pipeline, "kb")))
        .build()?;

    // The agent now calls rag_search automatically when it needs knowledge base info
    adk_runner::Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

This example also depends on `adk-agent`, `adk-model` (`gemini`), `adk-runner`, `tokio`, and
`anyhow`.

| `RagTool` | Value |
|-----------|-------|
| Name | `rag_search` |
| Parameters | `query` (required), `top_k` (overrides the configured value; the reranker and similarity threshold still apply), and `collection` only when the tool has allowed collections (see [Multiple Collections](#multiple-collections)) |
| Collections | The collection given to `RagTool::new`, plus any given to `with_allowed_collections`; the model cannot reach any other |
| Returns | An array of `{"id", "document_id", "text", "metadata", "score"}` results, best first; embedding vectors are not included |
| Invalid arguments | A missing `query`, a `top_k` that is not a positive integer, or a `collection` the tool does not allow fails the call with `tool.invalid_arguments`, whose message the model receives |
| Effect | `ToolEffect::ReadOnly` and concurrency-safe, so `DeclarativePolicy::allow_read_only()` admits it and `ToolExecutionStrategy::Auto` overlaps it with adjacent read-only calls |

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
| `FixedSizeChunker` | General text, logs | Every N bytes with overlap, on character boundaries |
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

Sizes are bytes of UTF-8, not characters, and chunks never split a character.
`new` normalises an invalid size pair (a zero `chunk_size` becomes 1 and a
`chunk_overlap` that is not smaller than `chunk_size` is ignored, with a
warning); `try_new` rejects it with `RagError::ConfigError`:

```rust
use adk_rag::FixedSizeChunker;

let chunker = FixedSizeChunker::try_new(512, 100)?;
assert!(FixedSizeChunker::try_new(100, 100).is_err());
```

`RecursiveChunker` is the best default choice — it tries paragraph breaks first, then sentence boundaries, then word boundaries, producing more natural chunks than fixed-size splitting.

`MarkdownChunker` adds a `header_path` metadata field to each chunk (e.g. `"Getting Started > Installation"`), which helps the agent cite specific sections.

---

## Configuration

```rust
use adk_rag::RagConfig;

let config = RagConfig::builder()
    .chunk_size(256)            // max bytes per chunk (default: 512)
    .chunk_overlap(50)          // bytes shared by adjacent chunks (default: 100)
    .top_k(5)                   // results to return (default: 10)
    .similarity_threshold(0.5)  // minimum score to include (default: 0.0)
    .build()?;
```

| Parameter | What it controls | Guidance |
|-----------|-----------------|----------|
| `chunk_size` | Validated only (must be greater than 0) | The pipeline chunks with its chunker, so set the sizes on the chunker |
| `chunk_overlap` | Validated only (must be smaller than `chunk_size`) | As above |
| `top_k` | Number of results returned (must be greater than 0) | More results = more context for the LLM but higher token usage |
| `similarity_threshold` | Minimum score to include | 0.0 returns everything; 0.3–0.7 filters weak matches |

`build()` returns `RagError::ConfigError` when a rule above is broken.

---

## Embedding Providers

| Provider | Feature flag | Model | Requires |
|----------|-------------|-------|----------|
| `GeminiEmbeddingProvider` | `gemini` | `gemini-embedding-2` (3072 dimensions) | An API key |
| `OpenAIEmbeddingProvider` | `openai` | `text-embedding-3-small` (1536 dimensions) | An API key, or `OPENAI_API_KEY` with `from_env()` |

```rust
// Gemini
use adk_rag::GeminiEmbeddingProvider;
let embedder = GeminiEmbeddingProvider::new(&api_key)?
    .with_output_dimensionality(768);

// OpenAI
use adk_rag::OpenAIEmbeddingProvider;
let embedder = OpenAIEmbeddingProvider::new(&api_key)?
    .with_model("text-embedding-3-large")
    .with_dimensions(1024);
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
| `SurrealVectorStore` | `surrealdb` | SurrealDB in memory, on RocksDB, or remote |
| `AgentRetrievalStore` | `agent-retrieval` | Vertex AI Vector Search 2.0, with hybrid search |

```rust
// In-memory (no setup needed)
use adk_rag::InMemoryVectorStore;
let store = InMemoryVectorStore::new();

// Qdrant (requires running Qdrant server)
use adk_rag::QdrantVectorStore;
let store = QdrantVectorStore::new("http://localhost:6334")?;

// pgvector (requires PostgreSQL with pgvector extension)
use adk_rag::PgVectorStore;
let store = PgVectorStore::new("postgres://user:pass@localhost/db").await?;

// LanceDB (a local directory)
use adk_rag::LanceDBVectorStore;
let store = LanceDBVectorStore::new("./lancedb").await?;

// SurrealDB (embedded, in memory)
use adk_rag::SurrealVectorStore;
let store = SurrealVectorStore::in_memory().await?;
```

The `vertex-rag` feature adds `VertexRagEngineClient` and `VertexAiRagRetrievalTool`, which
retrieve from a Vertex AI RAG Engine corpus instead of a local pipeline.

> **Note:** `PgVectorStore` stores each collection in the table `rag_<name>`. A
> collection name must be 1 to 59 bytes of ASCII lowercase letters, digits and
> underscores; any other name returns `RagError::VectorStoreError` instead of being
> rewritten, so two distinct names never share a table.

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

Use separate collections for different knowledge domains. A `RagTool` searches only the
collections it is configured with — the model never names a collection the application did not
give it.

| Configuration | Schema | A call naming another collection |
|---------------|--------|----------------------------------|
| `RagTool::new(pipeline, "docs")` | No `collection` argument | Fails with `tool.invalid_arguments`: the tool searches only `docs` |
| `RagTool::new(pipeline, "docs").with_allowed_collections(["faq"])` | `collection` lists `docs` and `faq`; omitted, it searches `docs` | Fails with `tool.invalid_arguments`, naming `docs` and `faq` |

Give one tool several collections when the model should choose among them:

```rust
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use adk_rag::{RagPipeline, RagTool};

async fn support_agent(
    pipeline: Arc<RagPipeline>,
    model: GeminiModel,
) -> anyhow::Result<adk_agent::LlmAgent> {
    // Create collections for different content types
    pipeline.create_collection("docs").await?;
    pipeline.create_collection("faq").await?;
    pipeline.create_collection("changelog").await?;

    // "docs" by default; the model may also choose "faq" or "changelog"
    let search = RagTool::new(pipeline, "docs").with_allowed_collections(["faq", "changelog"]);

    let agent = LlmAgentBuilder::new("support")
        .instruction("Search 'docs' for how-to questions, 'faq' for common questions.")
        .model(Arc::new(model))
        .tool(Arc::new(search))
        .build()?;
    Ok(agent)
}
```

### One collection per tenant

When each tenant has its own collection, build the tool per request with that tenant's
collection, and do not add other tenants' collections to `with_allowed_collections`:

```rust
use std::sync::Arc;

use adk_rag::{RagPipeline, RagTool};

fn tenant_search(pipeline: Arc<RagPipeline>, tenant_id: &str) -> RagTool {
    // The tenant comes from the authenticated request, never from the model.
    RagTool::new(pipeline, format!("kb_{tenant_id}"))
}
```

A call that names another tenant's collection fails before the vector store is queried.

---

## Feature Flags

Only pull the dependencies you need:

| Feature | Enables | Extra dependency |
|---------|---------|-----------------|
| *(default)* | Core traits, `InMemoryVectorStore`, all chunkers | none |
| `gemini` | `GeminiEmbeddingProvider` | `adk-gemini` |
| `openai` | `OpenAIEmbeddingProvider` | `reqwest` |
| `qdrant` | `QdrantVectorStore` | `qdrant-client` |
| `lancedb` | `LanceDBVectorStore` | `lancedb` |
| `pgvector` | `PgVectorStore` | `sqlx` |
| `surrealdb` | `SurrealVectorStore` | `surrealdb` |
| `agent-retrieval` | `AgentRetrievalStore` | `adk-gcp`, Google Cloud auth |
| `vertex-rag` | `VertexRagEngineClient`, `VertexAiRagRetrievalTool` | `adk-gcp`, Google Cloud auth |
| `full` | `gemini`, `openai`, `qdrant`, `lancedb`, `pgvector`, `surrealdb` | all of those |

```toml
# Just core
adk-rag = "3.0.0"

# With Gemini embeddings
adk-rag = { version = "3.0.0", features = ["gemini"] }

# Everything
adk-rag = { version = "3.0.0", features = ["full"] }
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
cargo adk new rag_agent --template rag
cd rag_agent
cargo run
```

---

## Best Practices

| Practice | Why |
|----------|-----|
| Use `RecursiveChunker` as default | Produces natural chunk boundaries |
| Keep chunks 200–500 bytes | Balances precision and context |
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
