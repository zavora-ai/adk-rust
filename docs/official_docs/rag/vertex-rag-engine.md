# Vertex AI RAG Engine

Retrieve grounded context from managed Vertex AI RAG Engine corpora — no
self-hosted vector store, embedding provider, or ingestion pipeline required.

---

## What it is

Vertex AI RAG Engine is Google Cloud's managed RAG backend: you import
documents into a **RAG corpus** and the platform handles chunking, embedding,
and vector search. `adk-rag`'s `vertex-rag` feature provides:

- **`VertexRagEngineClient`** — an ADC-authenticated, read-only data-plane
  client: get/list corpora, list imported files, and retrieve contexts
- **`VertexAiRagRetrievalTool`** — retrieval as an `adk_core::Tool`, the Rust
  analog of adk-python's `VertexAiRagRetrieval`

**Scope:** retrieval only. Corpus creation and file import are provisioning
concerns — use the [Vertex AI console](https://console.cloud.google.com/vertex-ai/rag)
or the `RagCorpora`/`RagFiles` management APIs.

---

## Installation

```toml
[dependencies]
adk-rag = { version = "2.1.0", features = ["vertex-rag"] }
```

Authentication uses Application Default Credentials:

```bash
gcloud auth application-default login
```

---

## Client

```rust
use adk_rag::vertex_rag::{RetrieveContextsRequest, VertexRagConfig, VertexRagEngineClient};

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    // Or VertexRagConfig::from_env() reading GOOGLE_CLOUD_PROJECT / GOOGLE_CLOUD_LOCATION
    let config = VertexRagConfig::new("my-project", "us-central1");
    let client = VertexRagEngineClient::new_with_adc(config)?;

    // Verify the corpus exists and has imported files; fails with
    // actionable guidance when it is missing, empty, or in ERROR state.
    let corpus = client.ensure_corpus_ready("1234567890").await?;
    println!("corpus: {:?} ({:?} files)", corpus.display_name, corpus.rag_files_count);

    // Enumerate what's in the project and the corpus.
    let corpora = client.list_corpora().await?;
    let files = client.list_rag_files("1234567890").await?;
    println!("{} corpora, {} files", corpora.len(), files.len());

    // Retrieve the most relevant passages for a query.
    let request = RetrieveContextsRequest::new("what is the refund policy?", ["1234567890"])
        .similarity_top_k(5)
        .vector_distance_threshold(0.7);
    for context in client.retrieve_contexts(&request).await? {
        println!(
            "[{:.3}] {} — {}",
            context.score.unwrap_or_default(),
            context.source_display_name.as_deref().unwrap_or("<unknown>"),
            context.text.as_deref().unwrap_or(""),
        );
    }
    Ok(())
}
```

Corpora may be passed as bare IDs (resolved against the client's project and
location) or full `projects/*/locations/*/ragCorpora/*` resource names.

> **Note:** `similarity_top_k` and `vector_distance_threshold` keep
> adk-python's names but are sent on the current wire path —
> `query.ragRetrievalConfig.topK` and
> `query.ragRetrievalConfig.filter.vectorDistanceThreshold`. The deprecated
> v1beta1 spellings (`query.similarityTopK`,
> `vertexRagStore.vectorDistanceThreshold`) were removed from v1 and are never
> emitted. `vector_similarity_threshold` is the filter's other, mutually
> exclusive arm.

---

## Retrieval tool

`VertexAiRagRetrievalTool` takes a single required `query` string and returns
a JSON array of `{text, sourceUri, sourceDisplayName, score}` objects. It
reports itself read-only and concurrency-safe, so
`ToolExecutionStrategy::Auto` may dispatch it in parallel with other reads.

```rust
use std::sync::Arc;
use adk_agent::LlmAgentBuilder;
use adk_model::GeminiModel;
use adk_rag::vertex_rag::{VertexAiRagRetrievalTool, VertexRagConfig, VertexRagEngineClient};

fn main() -> anyhow::Result<()> {
    let config = VertexRagConfig::new("my-project", "us-central1");
    let client = Arc::new(VertexRagEngineClient::new_with_adc(config)?);

    let retrieval = VertexAiRagRetrievalTool::new(client, vec!["1234567890".into()])
        .similarity_top_k(5)
        .vector_distance_threshold(0.7);

    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let agent = LlmAgentBuilder::new("rag-assistant")
        .description("Answers questions grounded in a Vertex AI RAG Engine corpus")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?))
        .tool(Arc::new(retrieval))
        .instruction(
            "Answer using the vertex_rag_retrieval tool. Retrieve first, then \
             answer strictly from the retrieved passages, citing sourceDisplayName.",
        )
        .build()?;
    let _ = agent;
    Ok(())
}
```

See [`examples/vertex_rag`](https://github.com/zavora-ai/adk-rust/tree/main/examples/vertex_rag)
for the full runnable agent:

```bash
cargo run --manifest-path examples/vertex_rag/Cargo.toml
```

---

## API reference

| Operation | Endpoint | Returns |
|-----------|----------|---------|
| `get_corpus(corpus)` | `GET v1beta1/{corpus}` | `RagCorpus` (404 becomes an actionable not-found error) |
| `ensure_corpus_ready(corpus)` | `GET v1beta1/{corpus}` | `RagCorpus`; errors when missing, empty, or `ERROR` state |
| `list_corpora()` | `GET v1beta1/{parent}/ragCorpora` | `Vec<RagCorpus>`, pagination followed |
| `list_rag_files(corpus)` | `GET v1beta1/{corpus}/ragFiles` | `Vec<RagFile>`, pagination followed |
| `retrieve_contexts(&request)` | `POST v1beta1/{parent}:retrieveContexts` | `Vec<RagContext>` |

Response types deserialize leniently — every field is optional and unknown
fields are ignored, so new server fields cannot break parsing. Errors carry
component `Memory` (retrieval is the memory domain; `AdkError` has no
dedicated RAG component), provider `vertex_ai`, and machine-readable
`rag.vertex.*` codes.

---

## Environment variables

| Variable | Used by | Description |
|----------|---------|-------------|
| `GOOGLE_CLOUD_PROJECT` | `VertexRagConfig::from_env` | Project that owns the corpora |
| `GOOGLE_CLOUD_LOCATION` | `VertexRagConfig::from_env` | Region, e.g. `us-central1` |
| `VERTEX_RAG_CORPUS` | example / live tests | Corpus ID or full resource name |

---

## Self-hosted RAG instead?

For a pipeline you run yourself — pluggable chunkers, embedding providers,
and vector stores (Qdrant, LanceDB, pgvector, SurrealDB) — see
[RAG](../tools/rag.md).
