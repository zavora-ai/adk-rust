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
use std::sync::Arc;
use adk_rag::{
    Document, FixedSizeChunker, InMemoryVectorStore,
    RagConfig, RagPipeline, EmbeddingProvider, Scope,
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
    let document = Document::new(
        "intro",
        "Rust is a systems programming language focused on safety and speed.",
    )
    .with_source_uri("https://www.rust-lang.org/")
    .with_title("Rust")
    .with_metadata("topic", "rust");

    // Every read and write names a scope; an application with one tenant uses this one
    let rag = pipeline.scoped(Scope::single_tenant());
    rag.ingest("docs", &document).await?;

    // Query
    let results = rag.query("docs", "safe programming").await?;
    for r in &results {
        let source = r.chunk.source_uri.as_deref().unwrap_or("unknown source");
        println!("[{:.3}] {} ({source})", r.score, r.chunk.text);
    }
    Ok(())
}
```

**How it works**:
1. `FixedSizeChunker` splits the document into 256-byte chunks with 50-byte overlap — the
   chunker's own sizes decide chunking
2. `MockEmbedder` converts each chunk into a 64-dimensional vector
3. `InMemoryVectorStore` stores the vectors and searches by cosine similarity
4. `pipeline.scoped(scope)` binds ingest and query to one [scope](#scopes)
5. `query()` embeds the question, finds the closest chunks in the scope, and returns them ranked
   by score; `query_with()` takes `QueryOptions`, such as `QueryOptions::new().with_top_k(3)`

`Document` and `Chunk` are `#[non_exhaustive]`, so they are built with `Document::new(id, text)`
and `Chunk::new(id, document_id, text)` plus the `with_*` builders rather than struct literals.

### Provenance

A document's `source_uri` and `title` say where its text came from:

| Stage | What happens to `source_uri` and `title` |
|-------|------------------------------------------|
| Chunking | Every built-in chunker copies them onto each chunk (`Chunk::from_document` does it for a custom chunker) |
| Storage | Every vector store persists them as fields of their own, beside the metadata |
| Search | Every vector store returns them on `SearchResult::chunk` |
| `RagTool` | Each result carries them when the document had them, so the model can cite the source |

Metadata is stored verbatim with no reserved keys, so provenance never collides with a metadata
entry. Data stored before provenance existed returns `None` for both fields, except on LanceDB,
whose tables need re-creating (see [Vector Store Backends](#vector-store-backends)).

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
    RagConfig, RagPipeline, RagTool, RecursiveChunker, Scope,
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
    pipeline.scoped(Scope::single_tenant()).ingest(
        "kb",
        &Document::new("returns", "Our return policy allows returns within 30 days with a receipt.")
            .with_source_uri("https://example.com/help/returns")
            .with_title("Returns policy"),
    ).await?;

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
| Scope | `Scope::single_tenant()` unless `with_scope(scope)` fixes another, `with_scope_from(\|ctx\| ..)` resolves it from the call's context, or `with_scope_per_user()` uses `[app_name, user_id]`; the model cannot name a scope (see [Scopes](#scopes)) |
| Filters | `with_policy_filter(filter)` applies to every call; `with_model_filterable([..])` adds a `filter` argument limited to the listed metadata keys, AND-ed with the policy (see [Filters and deletes](#filters-and-deletes)) |
| Returns | An array of `{"id", "document_id", "text", "metadata", "score"}` results, best first, each with `source_uri` and `title` when its document had them; embedding vectors are not included |
| Grounding | Each result with a `source_uri` is recorded as a `GroundingSource` on the call's event, so the agent binds the answer's `[title](uri)` links to it and `CitationRequired` can check them (see [Guardrails](../security/guardrails.md#grounded-citations)) |
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

No backend rewrites a collection name onto another collection:

| Backend | Collection names |
|---------|------------------|
| `PgVectorStore` | 1 to 59 ASCII lowercase letters, digits, and underscores |
| `SurrealVectorStore` | One or more ASCII letters, digits, and underscores; case sensitive |
| `AgentRetrievalStore` | ASCII lowercase letters, digits, and underscores, starting with a letter and ending with a letter or digit; each underscore becomes a hyphen in the collection ID |
| `LanceDBVectorStore` | One or more ASCII letters, digits, underscores, hyphens, and periods; on a case-insensitive filesystem (the macOS and Windows defaults), names that differ only in case share a table |
| `InMemoryVectorStore`, `QdrantVectorStore` | Passed through unchanged; Qdrant refuses names it does not support |

A name outside a backend's rule returns `RagError::VectorStoreError`. Names of lowercase
letters, digits, and underscores that start with a letter work on every backend.

`QdrantVectorStore` stores each chunk under the UUID v5 of its chunk ID, because Qdrant accepts
only unsigned integers and UUIDs as point IDs, and keeps the chunk ID in the `chunk_id` payload
field that search reads back.

Every backend stores a chunk's `source_uri` and `title` as fields of their own:

| Backend | Storage | Data stored before provenance |
|---------|---------|-------------------------------|
| `InMemoryVectorStore` | The stored `Chunk` | — |
| `PgVectorStore` | `source_uri` and `title` columns | `create_collection` adds the columns in place; old rows return `None` |
| `LanceDBVectorStore` | `source_uri` and `title` columns | `upsert` fails with guidance: delete the collection, create it again, and re-ingest |
| `QdrantVectorStore` | `source_uri` and `title` payload fields | Old points return `None` until re-ingested |
| `SurrealVectorStore` | `source_uri` and `title` fields | `create_collection` defines the fields; old rows return `None` |
| `AgentRetrievalStore` | `sourceUri` and `title` data fields | Old data objects return `None` until re-ingested |

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

### Scopes

Every `VectorStore` operation names a `Scope`, and a stored chunk is identified by its
collection, its scope, and its chunk ID together. Tenants can share one collection:

| Guarantee | What it means |
|-----------|---------------|
| Identity | The same document ID ingested in two scopes keeps two sets of chunks; neither overwrites the other |
| Search | A search sees only its scope's chunks. Every backend applies the scope inside its search, before the top `k` are chosen, so a tenant with one chunk still finds it beside a tenant with millions of nearer ones |
| Delete | `delete`, `count`, and `purge_scope` act on the scope's chunks only |
| No bypass | `RagPipeline` has no unscoped `ingest` or `query`; they live on `ScopedRag`, from `pipeline.scoped(scope)` |

`Scope::new(key)` takes an opaque key of 1 to 256 bytes without control characters.
`Scope::from_parts(["support-app", tenant_id])` percent-encodes each part and joins them with
`/`, so distinct part lists always give distinct scopes. `Scope::single_tenant()` is the scope
of an application with one tenant.

```rust
use std::sync::Arc;

use adk_rag::{Document, RagPipeline, RagTool, Scope};

async fn tenant_rag(
    pipeline: Arc<RagPipeline>,
    tenant_id: &str,
    document: &Document,
) -> adk_rag::Result<RagTool> {
    // The tenant comes from the authenticated request, never from the model.
    let scope = Scope::from_parts(["support-app", tenant_id])?;
    let rag = pipeline.scoped(scope.clone());
    rag.ingest("kb", document).await?;
    println!("tenant holds {} chunks", rag.count("kb").await?);
    Ok(RagTool::new(pipeline, "kb").with_scope(scope))
}
```

`ScopedRag::purge_scope(collection)` deletes every chunk of the scope and returns how many there
were, leaving other scopes untouched.

| Backend | How the scope is stored and applied |
|---------|-------------------------------------|
| `InMemoryVectorStore` | A map per scope |
| `PgVectorStore` | A `scope` column; the primary key is `(scope, id)`, and `WHERE scope = $n` precedes `LIMIT` |
| `LanceDBVectorStore` | A `scope` column; a typed prefilter, and a merge-insert keyed on `scope` and `id` |
| `QdrantVectorStore` | A `scope` payload field indexed as Qdrant's tenant field; a `must` filter on search, count, and delete; the point ID is the UUID v5 of scope and chunk ID |
| `SurrealVectorStore` | `scope` and `chunk_id` fields, an index on `scope`, and the condition beside the KNN operator; the record ID combines scope and chunk ID |
| `AgentRetrievalStore` | A `scope` data field; the `filter` of every search and query; the Data Object ID is derived from scope and chunk ID |

### Filters and deletes

A `Filter` narrows a search, a count, or a delete to the chunks it matches, inside the scope.
Every backend compiles it into its own query language with bound parameters or typed
expressions and applies it before the top `k` are chosen, so a selective filter still returns
its nearest matches. The metadata keys a filter may name are declared when the collection is
created:

```rust
use adk_rag::{Document, Field, Filter, QueryOptions, RagPipeline, Scope};

async fn retire_and_search(pipeline: &RagPipeline) -> adk_rag::Result<()> {
    pipeline.create_collection_with_filterable("kb", ["lang", "status"]).await?;
    let rag = pipeline.scoped(Scope::from_parts(["support-app", "tenant-42"])?);

    rag.ingest(
        "kb",
        &Document::new("refunds", "Refunds take 30 days.")
            .with_metadata("lang", "en")
            .with_metadata("status", "live"),
    )
    .await?;

    let english = Filter::and([
        Filter::eq(Field::metadata("lang"), "en"),
        !Filter::eq(Field::metadata("status"), "retired"),
    ]);
    let results = rag.query_with("kb", "refund window", &QueryOptions::new().with_filter(english)).await?;
    println!("{} results", results.len());

    let retired = Filter::eq(Field::metadata("status"), "retired");
    let deleted = rag.delete_where("kb", &retired).await?;
    let removed = rag.delete_document("kb", "refunds").await?;
    println!("deleted {deleted} retired chunks and {removed} chunks of one document");
    Ok(())
}
```

| Operation | Behaviour |
|-----------|-----------|
| `query_with(.., QueryOptions::new().with_filter(f))` | Searches the scope's chunks `f` matches |
| `count_where(collection, &f)` | Counts the scope's chunks `f` matches |
| `delete_where(collection, &f)` | Deletes the scope's chunks `f` matches and returns how many; other scopes are untouched |
| `delete_document(collection, id)` | `delete_where` on `Field::DocumentId` |
| `purge_scope(collection)` | The only operation that deletes a whole scope; an empty `And`, `Or`, or `In` is refused, never read as "everything" |
| `ingest(collection, &document)` | Replaces the document's chunks: upserts the new ones, then deletes those the new version no longer has |

A filter that names an undeclared metadata key, is malformed, or has a shape the backend cannot
express fails with `RagError::UnsupportedFilter` before any I/O. Agent Retrieval's filter language
has no `$exists` or `$not` and nests two levels, so it refuses `Exists`, negations other than
`!Eq` / `!In` on the document or chunk ID, and deeper nesting.

`RagTool` applies the same filters for an agent: a policy filter the application sets, and a
`filter` argument the model may use only on the keys the application lists:

```rust
use std::sync::Arc;

use adk_rag::{Field, Filter, RagPipeline, RagTool};

fn public_search(pipeline: Arc<RagPipeline>) -> RagTool {
    RagTool::new(pipeline, "kb")
        .with_scope_per_user()
        .with_policy_filter(Filter::eq(Field::metadata("status"), "live"))
        .with_model_filterable(["lang"])
}
```

> **Note:** a LanceDB delete is logical: deleted rows stay in the table's files and older
> versions. Erasure that must remove the bytes calls `LanceDBVectorStore::purge_deleted`
> afterwards, while no other process writes to the collection.

### One collection per tenant

Collections still separate tenants when each has its own. Build the tool per request with that
tenant's collection, and do not add other tenants' collections to `with_allowed_collections`:

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
