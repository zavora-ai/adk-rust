# Agent Retrieval Vector Store

Agent Retrieval (formerly Vector Search 2.0) is Google's managed vector database on the Gemini Enterprise Agent Platform, generally available at `vectorsearch.googleapis.com`. The `agent-retrieval` feature of `adk-rag` implements the [`VectorStore`] trait against it, so it slots into the existing RAG pipeline exactly like the pgvector, Qdrant, LanceDB, and SurrealDB backends.

```toml
[dependencies]
adk-rag = { version = "2.1.0", features = ["agent-retrieval"] }
```

## Quick start

```rust,no_run
use adk_rag::VectorStore;
use adk_rag::agent_retrieval::{AgentRetrievalConfig, AgentRetrievalStore};

async fn run() -> adk_rag::Result<()> {
    // Application Default Credentials; project/location from the environment.
    let store = AgentRetrievalStore::new_with_adc(AgentRetrievalConfig::from_env()?)?;

    store.create_collection("docs", 768).await?;
    // upsert / search / delete exactly as with any other VectorStore backend
    Ok(())
}
```

## Mapping onto the platform

| Trait operation | Agent Retrieval call |
|-----------------|----------------------|
| `create_collection` | `POST {parent}/collections` (long-running operation; `ALREADY_EXISTS` is a no-op) |
| `delete_collection` | `DELETE {collection}` (long-running operation; `NOT_FOUND` is a no-op) |
| `upsert` | `dataObjects:batchCreate` (≤1000, atomic), falling back to per-object create-else-patch for last-write-wins |
| `delete` | `dataObjects:batchDelete`, falling back to per-object delete when IDs are missing |
| `search` | `dataObjects:search` with a `vectorSearch` query |

Chunk text and metadata are stored **in** the Data Object alongside the vector — no companion store is needed. The store runs in BYOE (bring-your-own-embeddings) mode: embeddings come from adk-rag's embedder pipeline, keeping behavior identical across backends. Scores are `DOT_PRODUCT` distances, which equal cosine similarity for normalized embeddings (higher is more relevant).

## Configuration

| Option | Effect |
|--------|--------|
| `with_collection_prefix("rag-")` | Prefixes every collection ID, isolating this store's collections |
| `with_existing_collections_only(true)` | `create_collection` validates the collection exists instead of creating it — for deployments that pre-provision with platform tooling |
| `with_endpoint(...)` | Custom API origin (loopback HTTP allowed for tests) |

Collection names are sanitized into RFC 1035 labels (lowercased; invalid characters become hyphens), so `my_docs` becomes collection ID `my-docs`. Distinct names that sanitize identically collide — choose names that differ in more than case or punctuation.

## Beyond the trait

For callers holding the concrete `AgentRetrievalStore`, `hybrid_search` runs a semantic and a keyword query in one `dataObjects:batchSearch` call and returns the reciprocal-rank-fusion ranked list:

```rust,no_run
# use adk_rag::agent_retrieval::{AgentRetrievalConfig, AgentRetrievalStore};
# async fn run(store: AgentRetrievalStore, embedding: Vec<f32>) -> adk_rag::Result<()> {
let results = store
    .hybrid_search("docs", &embedding, "quarterly revenue", 10, (0.6, 0.4))
    .await?;
# Ok(())
# }
```

## Platform notes

- **Regions:** Agent Retrieval is regional; supported regions include `us-central1`, `us-east4`, `us-west1`, `europe-north1`, `europe-west2`, `europe-west4`, `asia-east1`, `asia-northeast1`, and `asia-southeast1`.
- **CMEK:** standalone Agent Retrieval supports customer-managed encryption keys via the collection's `encryptionSpec` (immutable at creation). Pre-provision CMEK collections with platform tooling and use `with_existing_collections_only(true)`.
- **Vector Search 1.0 is deliberately not implemented.** Its index/endpoint infrastructure model fits the `VectorStore` trait poorly, and Google positions Agent Retrieval as its successor.
- **Quotas:** batch writes are capped at 1000 objects per request and are atomic; the store chunks larger upserts automatically.

## Testing

The backend passes the shared `VectorStore` contract and property suite that all adk-rag backends run, so it is behaviorally interchangeable with the other stores. The `#[ignore]` live test creates and deletes its own uniquely-named collection (data-plane, like the pgvector and Qdrant live tests):

```bash
GOOGLE_CLOUD_PROJECT=my-project GOOGLE_CLOUD_LOCATION=us-central1 \
  cargo test -p adk-rag --features agent-retrieval -- --ignored
```
