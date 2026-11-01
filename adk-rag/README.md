# adk-rag

Give your AI agents a knowledge base. `adk-rag` adds Retrieval-Augmented Generation (RAG) to [ADK-Rust](https://github.com/zavora-ai/adk-rust) so your agents can search documents and answer questions using your own data.

[![Crates.io](https://img.shields.io/crates/v/adk-rag.svg)](https://crates.io/crates/adk-rag)
[![Documentation](https://docs.rs/adk-rag/badge.svg)](https://docs.rs/adk-rag)
[![License](https://img.shields.io/crates/l/adk-rag.svg)](../LICENSE)


## ADK RAG
The `adk-rag` crate provides Retrieval-Augmented Generation capabilities for the ADK-Rust workspace. It offers a modular, trait-based architecture for document chunking, embedding generation, vector storage, similarity search, reranking, and agentic retrieval. The crate follows the ADK-Rust conventions of feature-gated backends, async-trait interfaces, and builder-pattern configuration. It integrates with existing ADK crates (`adk-gemini` for embeddings, `adk-core` for the Tool trait) and supports multiple vector store backends (in-memory, Qdrant, LanceDB, pgvector, SurrealDB, Agent Retrieval), plus managed retrieval from Vertex AI RAG Engine corpora.

## What is RAG?

RAG stands for Retrieval-Augmented Generation. Instead of relying only on what an LLM was trained on, RAG lets your agent look up relevant information from your documents before answering:

1. **Ingest** — Documents are split into chunks, converted to vector embeddings, and stored
2. **Query** — A user question is embedded and matched against stored chunks
3. **Generate** — The most relevant chunks are passed to the LLM as context

## Quick Start

The fastest way to get a working RAG pipeline. Uses Gemini for embeddings (free API key from [Google AI Studio](https://aistudio.google.com/apikey)).

```toml
[dependencies]
adk-rag = { version = "3.0.0", features = ["gemini"] }
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

    let document = Document::new(
        "intro",
        "Rust is a systems programming language focused on safety and speed.",
    )
    .with_source_uri("https://www.rust-lang.org/")
    .with_title("Rust");

    // Every read and write names a scope; an application with one tenant uses this one.
    let rag = pipeline.scoped(Scope::single_tenant());
    rag.ingest("docs", &document).await?;

    let results = rag.query("docs", "safe programming language").await?;
    for r in &results {
        let source = r.chunk.source_uri.as_deref().unwrap_or("unknown source");
        println!("[score: {:.3}] {} ({source})", r.score, r.chunk.text);
    }
    Ok(())
}
```

```bash
GOOGLE_API_KEY=your-key-here cargo run
```

## Agent with RAG Tool

An agent that searches a knowledge base to answer questions. The agent decides when to call
`rag_search` and answers from the retrieved chunks.

```toml
[dependencies]
adk-rag = { version = "3.0.0", features = ["gemini"] }
adk-agent = "3.0.0"
adk-core = "3.0.0"
adk-model = "3.0.0"
adk-runner = "3.0.0"
adk-session = "3.0.0"
futures = "0.3"
tokio = { version = "1", features = ["full"] }
```

```rust
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::Content;
use adk_model::GeminiModel;
use adk_rag::{
    Document, GeminiEmbeddingProvider, InMemoryVectorStore, RagConfig, RagPipeline, RagTool,
    RecursiveChunker, Scope,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;

    let pipeline = Arc::new(
        RagPipeline::builder()
            .config(RagConfig::builder().chunk_size(300).chunk_overlap(50).top_k(3).build()?)
            .embedding_provider(Arc::new(GeminiEmbeddingProvider::new(&api_key)?))
            .vector_store(Arc::new(InMemoryVectorStore::new()))
            .chunker(Arc::new(RecursiveChunker::new(300, 50)))
            .build()?,
    );
    pipeline.create_collection("kb").await?;
    pipeline
        .scoped(Scope::single_tenant())
        .ingest(
            "kb",
            &Document::new("faq", "Our return policy allows returns within 30 days with a receipt.")
                .with_source_uri("https://example.com/help/returns")
                .with_title("Returns FAQ"),
        )
        .await?;

    let agent = LlmAgentBuilder::new("support_agent")
        .instruction("Answer questions using the rag_search tool.")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?))
        .tool(Arc::new(RagTool::new(pipeline, "kb")))
        .build()?;

    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "support".into(),
            user_id: "user1".into(),
            session_id: Some("s1".into()),
            state: Default::default(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name("support")
        .agent(Arc::new(agent))
        .session_service(sessions)
        .build()?;

    let question = Content::new("user").with_text("How many days do I have to return an item?");
    let mut events = runner.run_str("user1", "s1", question).await?;
    while let Some(event) = events.next().await {
        let event = event?;
        // Streamed deltas arrive as partial events before the complete response.
        if event.llm_response.partial {
            continue;
        }
        if let Some(content) = event.llm_response.content {
            for part in &content.parts {
                if let Some(text) = part.text() {
                    print!("{text}");
                }
            }
        }
    }
    println!();
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

The `RagPipeline` wires these together. The `RagTool` wraps the pipeline as an `adk_core::Tool` so any ADK agent can call it. `AgentRetrievalStore` (`agent-retrieval` feature) is a managed `VectorStore` on Google's Agent Retrieval service.

## Embedding Providers

### Gemini (recommended)

Uses Google's `gemini-embedding-2` model (3072 dimensions).

```toml
adk-rag = { version = "3.0.0", features = ["gemini"] }
```

```rust
use adk_rag::{EmbeddingProvider, GeminiEmbeddingProvider};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = GeminiEmbeddingProvider::new(std::env::var("GOOGLE_API_KEY")?)?;
    let embedding = provider.embed("Rust is memory safe.").await?;
    assert_eq!(embedding.len(), provider.dimensions());
    Ok(())
}
```

`with_output_dimensionality(dims)` requests shorter vectors and `with_task_type(task)` sets the
embedding task.

### OpenAI

Uses `text-embedding-3-small` (1536 dimensions) by default. Supports dimension truncation via Matryoshka.

```toml
adk-rag = { version = "3.0.0", features = ["openai"] }
```

```rust
use adk_rag::{EmbeddingProvider, OpenAIEmbeddingProvider};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Reads OPENAI_API_KEY; `OpenAIEmbeddingProvider::new(key)` takes the key directly.
    let provider = OpenAIEmbeddingProvider::from_env()?
        .with_model("text-embedding-3-large")
        .with_dimensions(256);
    let embedding = provider.embed("Rust is memory safe.").await?;
    assert_eq!(embedding.len(), 256);
    Ok(())
}
```

### Custom Embedding Provider

Implement the `EmbeddingProvider` trait to use any embedding model — a local model, a different API, or a mock for testing.

```rust
use adk_rag::{EmbeddingProvider, Result};
use async_trait::async_trait;

struct MyEmbedder;

#[async_trait]
impl EmbeddingProvider for MyEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        // Call your embedding model here.
        Ok(vec![text.len() as f32; 384])
    }

    fn dimensions(&self) -> usize {
        384
    }
}
```

`embed_batch` has a default implementation that calls `embed` for each text; override it when
your model accepts batches.

Add `async-trait = "0.1"` to your `Cargo.toml` when implementing traits.

## Vector Stores

### InMemoryVectorStore (default)

No external dependencies. Good for development, testing, and small datasets. Data is lost when the process exits.

```rust
use adk_rag::InMemoryVectorStore;

fn main() {
    let store = InMemoryVectorStore::new();
    let _ = store;
}
```

### Qdrant

Production-ready vector database with filtering, snapshots, and clustering.

```toml
adk-rag = { version = "3.0.0", features = ["qdrant"] }
```

```rust
use adk_rag::QdrantVectorStore;

fn main() -> adk_rag::Result<()> {
    let store = QdrantVectorStore::new("http://localhost:6334")?;
    let _ = store;
    Ok(())
}
```

Qdrant accepts only unsigned integers and UUIDs as point IDs, so each chunk is stored under the
UUID v5 of its chunk ID and the chunk ID is kept in the point's `chunk_id` payload field. Search
returns the original chunk ID, and `delete` maps IDs the same way.

### LanceDB

Embedded vector database with no server required. Data persists to disk.

```toml
adk-rag = { version = "3.0.0", features = ["lancedb"] }
```

> Requires `protoc` installed: `brew install protobuf` (macOS), `apt install protobuf-compiler` (Ubuntu).

```rust
use adk_rag::LanceDBVectorStore;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = LanceDBVectorStore::new("/tmp/my-vectors").await?;
    let _ = store;
    Ok(())
}
```

`upsert` replaces rows by scope and chunk id, so re-ingesting a document updates
its chunks instead of duplicating them. An embedding whose length differs from the
collection's dimensions returns `RagError::VectorStoreError`.

### pgvector (PostgreSQL)

Use your existing PostgreSQL database for vector search.

```toml
adk-rag = { version = "3.0.0", features = ["pgvector"] }
```

```rust
use adk_rag::PgVectorStore;

#[tokio::main]
async fn main() -> adk_rag::Result<()> {
    let store = PgVectorStore::new("postgres://user:pass@localhost/mydb").await?;
    let _ = store;
    Ok(())
}
```

`PgVectorStore::from_pool(pool)` uses a pool the application already holds.

Each collection is stored in the table `rag_<name>`. A collection name must be 1
to 59 bytes of ASCII lowercase letters, digits and underscores, so the table name
fits PostgreSQL's 63-byte identifier limit. Any other name returns
`RagError::VectorStoreError` instead of being rewritten, so two distinct names
never share a table.

### SurrealDB

Embedded or remote multi-model database with built-in vector search.

```toml
adk-rag = { version = "3.0.0", features = ["surrealdb"] }
```

```rust
use adk_rag::SurrealVectorStore;

#[tokio::main]
async fn main() -> adk_rag::Result<()> {
    let in_memory = SurrealVectorStore::in_memory().await?;
    let on_disk = SurrealVectorStore::rocksdb("/tmp/surreal-data").await?;
    let _ = (in_memory, on_disk);
    Ok(())
}
```

`SurrealVectorStore::remote(url)` connects to a SurrealDB server.

Each collection is stored in the table of the same name. A collection name must be one or more
ASCII letters, digits, or underscores; any other name returns `RagError::VectorStoreError`
instead of being rewritten, so two distinct names never share a table. Table names are case
sensitive and quoted in queries, so `Docs`, `docs`, and SurrealQL keywords such as `select` are
distinct, valid names.

### Agent Retrieval

Google's managed vector database on the Gemini Enterprise Agent Platform (formerly Vector Search
2.0). The store keeps chunk text and metadata in each Data Object, takes embeddings from the
pipeline, and maps collection names to RFC 1035 collection IDs. A collection name uses ASCII
lowercase letters, digits, and underscores, starts with a letter (unless a collection prefix is
set), ends with a letter or digit, and fits 63 characters with the prefix; each underscore becomes
a hyphen, so `my_docs` is the collection `my-docs`. Any other name returns
`RagError::VectorStoreError` instead of being rewritten.

```toml
adk-rag = { version = "3.0.0", features = ["agent-retrieval"] }
```

```rust
use adk_rag::{AgentRetrievalConfig, AgentRetrievalStore};

#[tokio::main]
async fn main() -> adk_rag::Result<()> {
    // Application Default Credentials.
    let config = AgentRetrievalConfig::new("my-project", "us-central1").with_collection_prefix("support");
    let store = AgentRetrievalStore::new_with_adc(config)?;
    let _ = store;
    Ok(())
}
```

`AgentRetrievalStore::hybrid_search` fuses semantic and text search, both filtered by scope,
with reciprocal rank fusion. Each chunk is stored under a Data Object ID derived from its scope
and chunk ID, because Agent Retrieval accepts only RFC 1035 labels, and `create_collection`
declares a data schema because the service admits no undeclared data field.

## Vertex AI RAG Engine (managed)

Instead of running your own pipeline, retrieve from managed Vertex AI RAG
Engine corpora — the platform handles chunking, embedding, and vector search.
The `vertex-rag` feature provides a read-only data-plane client and a
retrieval tool (corpus creation and file import stay in the Vertex AI console
or management APIs):

```toml
adk-rag = { version = "3.0.0", features = ["vertex-rag"] }
```

```rust
use std::sync::Arc;

use adk_rag::vertex_rag::{VertexAiRagRetrievalTool, VertexRagConfig, VertexRagEngineClient};

#[tokio::main]
async fn main() -> adk_rag::Result<()> {
    let config = VertexRagConfig::new("my-project", "us-central1");
    let client = Arc::new(VertexRagEngineClient::new_with_adc(config)?);

    // Fails with actionable guidance when the corpus is missing or empty.
    client.ensure_corpus_ready("1234567890").await?;

    // An adk_core::Tool with one required `query` string; it returns a JSON array of
    // {text, sourceUri, sourceDisplayName, score}.
    let tool = VertexAiRagRetrievalTool::new(client, vec!["1234567890".into()])
        .similarity_top_k(5)
        .vector_distance_threshold(0.7);
    let _ = tool;
    Ok(())
}
```

See [docs/official_docs/rag/vertex-rag-engine.md](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/rag/vertex-rag-engine.md)
and the [`examples/vertex_rag`](https://github.com/zavora-ai/adk-rust/tree/main/examples/vertex_rag) example.

## Choosing a Chunker

| Chunker | Best for | How it splits |
|---------|----------|--------------|
| `FixedSizeChunker` | General text, logs | Every N bytes with overlap, on character boundaries |
| `RecursiveChunker` | Articles, docs, code | Paragraphs → sentences → words (natural boundaries) |
| `MarkdownChunker` | Markdown files, READMEs | By headers, preserving section hierarchy in metadata |

```rust
use adk_rag::{Chunker, Document, FixedSizeChunker, MarkdownChunker, RecursiveChunker};

fn main() {
    let document = Document::new(
        "guide",
        "# Setup\n\nInstall the toolchain.\n\n# Usage\n\nRun the binary.",
    );

    // Fixed: 512 bytes per chunk, 100 bytes of overlap.
    let fixed = FixedSizeChunker::new(512, 100);
    // Recursive: paragraph breaks first, then sentences, then words.
    let recursive = RecursiveChunker::new(512, 100);
    // Markdown: splits by headers and stores the header path in metadata.
    let markdown = MarkdownChunker::new(512, 100);

    for chunker in [&fixed as &dyn Chunker, &recursive, &markdown] {
        println!("{} chunks", chunker.chunk(&document).len());
    }
}
```

Sizes are bytes of UTF-8, not characters, and chunks never split a character.
`new` normalises an invalid size pair (a zero `chunk_size` becomes 1 and a
`chunk_overlap` that is not smaller than `chunk_size` is ignored, with a
warning); `try_new` rejects it with `RagError::ConfigError`:

```rust
use adk_rag::FixedSizeChunker;

fn main() -> adk_rag::Result<()> {
    let chunker = FixedSizeChunker::try_new(512, 100)?;
    assert!(FixedSizeChunker::try_new(100, 100).is_err());
    let _ = chunker;
    Ok(())
}
```

## Configuration

```rust
use adk_rag::RagConfig;

fn main() -> adk_rag::Result<()> {
    let config = RagConfig::builder()
        .chunk_size(256) // max bytes per chunk (default: 512)
        .chunk_overlap(50) // bytes shared by adjacent chunks (default: 100)
        .top_k(5) // number of results to return (default: 10)
        .similarity_threshold(0.5) // minimum score to include (default: 0.0)
        .build()?;
    assert_eq!(config.top_k, 5);
    Ok(())
}
```

- **chunk_size** — Smaller chunks are more precise but may lose context. 200–500 is a good range.
- **chunk_overlap** — Prevents information loss at chunk boundaries. 10–20% of chunk_size works well.
- **top_k** — More results give the LLM more context but increase token usage.
- **similarity_threshold** — Filter out low-quality matches. 0.0 returns everything, 0.3–0.7 keeps strong matches only.

## Writing a Custom Reranker

The default `NoOpReranker` passes results through unchanged. Write your own to improve precision:

```toml
[dependencies]
adk-rag = { version = "3.0.0", features = ["gemini"] }
async-trait = "0.1"
tokio = { version = "1", features = ["full"] }
```

```rust
use std::sync::Arc;

use adk_rag::{
    GeminiEmbeddingProvider, InMemoryVectorStore, RagConfig, RagPipeline, RecursiveChunker,
    Reranker, SearchResult,
};
use async_trait::async_trait;

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
        let keywords: Vec<String> =
            query.split_whitespace().filter(|w| w.len() > 3).map(str::to_lowercase).collect();

        for result in &mut results {
            let text = result.chunk.text.to_lowercase();
            let hits = keywords.iter().filter(|kw| text.contains(kw.as_str())).count();
            result.score += hits as f32 * self.boost;
        }

        results.sort_by(|a, b| b.score.total_cmp(&a.score));
        Ok(results)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pipeline = RagPipeline::builder()
        .config(RagConfig::default())
        .embedding_provider(Arc::new(GeminiEmbeddingProvider::new(std::env::var("GOOGLE_API_KEY")?)?))
        .vector_store(Arc::new(InMemoryVectorStore::new()))
        .chunker(Arc::new(RecursiveChunker::new(512, 100)))
        .reranker(Arc::new(KeywordBoostReranker { boost: 0.1 }))
        .build()?;
    let _ = pipeline;
    Ok(())
}
```

## Feature Flags

```toml
# Core only (in-memory store, all chunkers, no external deps)
adk-rag = "3.0.0"

# With Gemini embeddings (recommended)
adk-rag = { version = "3.0.0", features = ["gemini"] }

# With OpenAI embeddings
adk-rag = { version = "3.0.0", features = ["openai"] }

# With a persistent vector store
adk-rag = { version = "3.0.0", features = ["gemini", "qdrant"] }

# Everything
adk-rag = { version = "3.0.0", features = ["full"] }
```

| Feature | Enables | Extra dependency |
|---------|---------|-----------------|
| *(default)* | Core traits, `InMemoryVectorStore`, all chunkers | none |
| `gemini` | `GeminiEmbeddingProvider` | `adk-gemini` |
| `openai` | `OpenAIEmbeddingProvider` | `reqwest` |
| `onnx` | `OnnxEmbeddingProvider`: a sentence-transformer ONNX model (default `BAAI/bge-small-en-v1.5`, 384 dimensions) run in process, fetched once into the Hugging Face hub cache; `ort` downloads the onnxruntime binary at build time | `ort`, `tokenizers`, `hf-hub` |
| `qdrant` | `QdrantVectorStore` | `qdrant-client` |
| `lancedb` | `LanceDBVectorStore` | `lancedb`, `arrow` |
| `pgvector` | `PgVectorStore` | `sqlx` |
| `surrealdb` | `SurrealVectorStore` | `surrealdb` |
| `agent-retrieval` | `AgentRetrievalStore` (Agent Retrieval, formerly Vector Search 2.0) | `adk-gcp`, `google-cloud-auth` |
| `vertex-rag` | `VertexRagEngineClient`, `VertexAiRagRetrievalTool` | `adk-gcp`, `google-cloud-auth` |
| `full` | `gemini`, `openai`, `qdrant`, `lancedb`, `pgvector`, `surrealdb` | all of those |

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
        if norm > 0.0 {
            v.iter_mut().for_each(|x| *x /= norm);
        }
        Ok(v)
    }

    fn dimensions(&self) -> usize {
        64
    }
}
```

This produces stable vectors so your tests are reproducible, but the similarity scores won't be meaningful. Use it for testing pipeline wiring, not search quality.

## Examples

| Example | What it shows |
|---------|---------------|
| [`examples/vertex_rag`](https://github.com/zavora-ai/adk-rust/tree/main/examples/vertex_rag) | Vertex AI RAG Engine retrieval |

The [adk-playground](https://github.com/zavora-ai/adk-playground) repository has more RAG
examples, such as a custom embedder and multi-collection search.

## API Reference

### Core types

| Type | Fields |
|------|--------|
| `Document` | `id`, `text`, `metadata: HashMap<String, String>`, `source_uri: Option<String>`, `title: Option<String>` |
| `Chunk` | `id`, `text`, `embedding: Vec<f32>`, `metadata`, `document_id`, `source_uri`, `title` |
| `SearchResult` | `chunk: Chunk`, `score: f32` |

`Document` and `Chunk` are `#[non_exhaustive]`: build them with `Document::new(id, text)` and
`Chunk::new(id, document_id, text)` or `Chunk::from_document(&document, id, text)`, then the
`with_metadata`, `with_source_uri`, `with_title`, and (for `Chunk`) `with_embedding` builders.

### Provenance

A document's `source_uri` and `title` say where its text came from. Every built-in chunker
copies them onto each chunk, every vector store persists them as fields of their own and
returns them with search results, and `RagTool` passes them to the model. Metadata is stored
verbatim with no reserved keys, so provenance never collides with a metadata entry.

| Backend | Where provenance is stored | Existing data |
|---------|---------------------------|---------------|
| In-memory | the stored `Chunk` | — |
| pgvector | `source_uri` and `title` columns | `create_collection` adds the columns in place |
| LanceDB | `source_uri` and `title` columns | re-create the table and re-ingest; `upsert` into an old table fails with that guidance |
| Qdrant | `source_uri` and `title` payload fields | old points return `None` until re-ingested |
| SurrealDB | `source_uri` and `title` fields | `create_collection` defines the fields; old rows return `None` |
| Agent Retrieval | `sourceUri` and `title` data fields | old data objects return `None` until re-ingested |

### Scopes

Every `VectorStore` operation names a `Scope`, and a stored chunk is identified by its
collection, scope, and chunk ID together, so tenants can share a collection: the same
document ID in two scopes keeps two sets of chunks, and a search sees only its scope's chunks.
Every backend applies the scope inside its search, before the top `k` are chosen, so a small
tenant keeps its recall beside a large one. The shared contract suite checks all of this on
every backend, including a scope of one chunk beside 500 nearer chunks of another scope.

| Constructor | Key |
|-------------|-----|
| `Scope::new("org-7")` | The opaque key itself: 1 to 256 bytes, no control characters |
| `Scope::from_parts(["support", tenant_id])` | Each part percent-encoded, joined with `/` |
| `Scope::single_tenant()` | `:single-tenant`, for an application with one tenant |

### Pipeline methods

| Method | Does |
|--------|------|
| `create_collection(name)`, `delete_collection(name)` | Manage collections in the vector store |
| `create_collection_with_filterable(name, ["lang"])` | Create a collection whose listed metadata keys filters may name |
| `scoped(scope)` | A `ScopedRag` that ingests into, searches, and deletes from one scope |

| `ScopedRag` method | Does |
|--------------------|------|
| `ingest(collection, &document)` | Chunk, embed, and store one document, replacing its earlier chunks; returns its chunks |
| `ingest_batch(collection, &documents)` | The same for several documents |
| `query(collection, text)` | Embed, search the scope, rerank, and drop results below `similarity_threshold` |
| `query_with(collection, text, &options)` | The same with `QueryOptions`: `with_top_k(3)`, `with_filter(filter)` |
| `count(collection)`, `count_where(collection, &filter)` | The number of the scope's chunks, or of those the filter matches |
| `delete_where(collection, &filter)` | Delete the scope's chunks the filter matches; returns how many |
| `delete_document(collection, id)` | Delete every chunk of one document in the scope |
| `purge_scope(collection)` | Delete every chunk of the scope; returns how many it held |

`ingest` upserts the new chunks, then deletes the document's chunks in the scope that the new
version no longer has, so a shorter version leaves no stale chunks and a search never finds the
document without chunks. A document with empty text removes the document.

| Backend | Scope storage | Data stored before scopes |
|---------|---------------|---------------------------|
| In-memory | a map per scope | — |
| pgvector | `scope` column, primary key `(scope, id)` | `create_collection` migrates in place; existing rows join `Scope::single_tenant()` |
| LanceDB | `scope` column, merge-insert on `scope` and `id` | re-create and re-ingest; `upsert` fails with that guidance |
| Qdrant | `scope` payload field, tenant index; point ID from scope and chunk ID | re-ingest |
| SurrealDB | `scope` and `chunk_id` fields, index on `scope`; record ID from scope and chunk ID | re-ingest |
| Agent Retrieval | `scope` data field; Data Object ID from scope and chunk ID; declared data schema | re-create and re-ingest |

### Filters

A `Filter` is a typed predicate over a chunk's metadata, `document_id`, `source_uri`, and chunk
ID. Every backend compiles it into its own query language with bound parameters or typed
expressions and applies it with the scope, before the top `k` are chosen.

```rust
use adk_rag::{Field, Filter, QueryOptions};

let filter = Filter::and([
    Filter::eq(Field::metadata("lang"), "en"),
    Filter::is_in(Field::metadata("section"), ["billing", "refunds"]),
    !Filter::exists(Field::metadata("archived")),
]);
let options = QueryOptions::new().with_top_k(5).with_filter(filter);
```

| Filter | Matches |
|--------|---------|
| `Filter::eq(field, value)` | The field is present and equals the value |
| `Filter::is_in(field, values)` | The field is present and equals one of the values (at least one) |
| `Filter::exists(field)` | The field is present |
| `Filter::and([..])`, `Filter::or([..])` | Every / at least one filter matches (at least one filter) |
| `!filter` | The filter does not match, so `!Filter::eq(field, v)` matches chunks without the field |

Values are strings. A metadata key a filter names must be declared on the collection —
`create_collection_with_filterable(name, keys)`, or `CollectionSpec::with_filterable` on a
`VectorStore` — and use ASCII letters, digits, and underscores. The pipeline refuses an
undeclared key, an empty `And`, `Or`, or `In`, and any shape a backend cannot express with
`RagError::UnsupportedFilter` before any I/O; no backend ignores a predicate or applies part of
one. `Filter::matches` is the reference evaluator: the in-memory store uses it, and the shared
contract compares every other backend with it on a filter catalogue and on generated filter
trees.

| Backend | Compiles to | Declared keys |
|---------|-------------|---------------|
| In-memory | `Filter::matches` | — |
| pgvector | `WHERE` conditions; keys and values are bound parameters, leaves wrapped in `COALESCE(.., false)` | — |
| LanceDB | Typed DataFusion expressions, prefiltered | Nullable `m_<key>` columns |
| Qdrant | Nested `must` / `should` / `must_not` filters on `metadata.<key>` | Keyword payload indexes |
| SurrealDB | A condition beside the KNN operator; values are bound parameters | — |
| Agent Retrieval | `$eq`, `$in`, `$and`, `$or`; `$ne`/`$nin` for `!` around `Eq`/`In` on the document or chunk ID | `m_<key>` data fields in the schema |

Agent Retrieval refuses `Exists`, other negations, and nesting deeper than two levels (the
scope's `$and` counts as one), because its filter language cannot express them.

### RagTool

`RagTool::new(pipeline, default_collection)` exposes the pipeline as the `rag_search` tool,
searching that one collection in `Scope::single_tenant()`. `with_allowed_collections([...])`
lets the model choose among further collections:

| Configuration | Arguments | A call naming another collection |
|---------------|-----------|----------------------------------|
| `RagTool::new(pipeline, "docs")` | `query` (required), `top_k` | Fails with `tool.invalid_arguments` |
| `.with_allowed_collections(["faq"])` | `query` (required), `top_k`, `collection` (`docs` or `faq`; `docs` when omitted) | Fails with `tool.invalid_arguments`, naming the allowed collections |

```json
{ "query": "pricing info", "collection": "faq", "top_k": 5 }
```

| Builder | Effect |
|---------|--------|
| `with_scope(scope)` | Every call searches `scope` |
| `with_scope_from(\|ctx\| ..)` | Each call's scope is resolved from its `ToolContext` (app, user, session, state, identity); an error fails the call before any search |
| `with_scope_per_user()` | `Scope::from_parts([app_name, user_id])` of each call |
| `with_policy_filter(filter)` | `filter` applies to every call, AND-ed with the model's |
| `with_model_filterable(["section"])` | The schema gains a `filter` argument mapping each listed metadata key to a string (equal) or an array of strings (one of); any other key fails with `tool.invalid_arguments` |

The scope and policy filter are configuration, never model arguments, so the model can narrow
results but never reach another scope or widen past the policy. Keys of the policy filter and
model-filterable keys must be declared filterable on the collection.

```rust,ignore
use adk_rag::{Field, Filter, RagTool};

let tool = RagTool::new(pipeline, "kb")
    .with_scope_per_user()
    .with_policy_filter(Filter::eq(Field::metadata("visibility"), "public"))
    .with_model_filterable(["section"]);
// The model may call: {"query": "refund window", "filter": {"section": ["billing", "refunds"]}}
```

The tool returns the results best first, each as
`{"id", "document_id", "text", "metadata", "score"}` plus `source_uri` and `title` when the
chunk's document had them. Embedding vectors are left out, and a
`top_k` override is reranked and filtered like any other query. The tool is read-only and
concurrency-safe. With one collection per tenant, build one tool per tenant from the
authenticated request.

## SQLx compatibility

ADK-Rust 3.0 SQL backends use SQLx 0.9. SQLx types appear in this crate's public
API — `PgVectorStore::from_pool` — so an application that passes in its own pool must depend on
SQLx 0.9 too. Applications that let ADK open its own connections need no SQLx
dependency. The [SQLx 0.9 migration guide](../docs/official_docs/migration/sqlx-0.9.md)
lists who is affected and what changed, and its
[TLS behaviour](../docs/official_docs/migration/sqlx-0.9.md#tls-behaviour) section
covers PostgreSQL certificate verification.

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.
