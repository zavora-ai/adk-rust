# Agent Retrieval Vector Store

Agent Retrieval (formerly Vector Search 2.0) is Google's managed vector database on the Gemini Enterprise Agent Platform, served at `vectorsearch.googleapis.com`. The `agent-retrieval` feature of `adk-rag` implements the [`VectorStore`] trait against it, so it slots into the existing RAG pipeline exactly like the pgvector, Qdrant, LanceDB, and SurrealDB backends.

```toml
[dependencies]
adk-rag = { version = "3.0.0", features = ["agent-retrieval"] }
```

## Quick start

```rust,no_run
use adk_rag::agent_retrieval::{AgentRetrievalConfig, AgentRetrievalStore};
use adk_rag::{CollectionSpec, Scope, VectorQuery, VectorStore};

async fn run(embedding: Vec<f32>) -> adk_rag::Result<()> {
    // Application Default Credentials; project/location from the environment.
    let store = AgentRetrievalStore::new_with_adc(AgentRetrievalConfig::from_env()?)?;

    store.create_collection("docs", &CollectionSpec::new(768)).await?;
    // upsert / search / delete / count / purge_scope as with any other VectorStore backend
    let tenant = Scope::from_parts(["support", "tenant-42"])?;
    let results = store.search("docs", &tenant, &VectorQuery::new(&embedding, 5)).await?;
    println!("{} results", results.len());
    Ok(())
}
```

## Mapping onto the platform

| Trait operation | Agent Retrieval call |
|-----------------|----------------------|
| `create_collection` | `POST {parent}/collections` with a data schema (long-running operation; `ALREADY_EXISTS` is a no-op) |
| `delete_collection` | `DELETE {collection}` (long-running operation; `NOT_FOUND` is a no-op) |
| `upsert` | `dataObjects:batchCreate` (≤1000, atomic), falling back to per-object create-else-patch for last-write-wins |
| `delete` | `dataObjects:batchDelete`, falling back to per-object delete when IDs are missing |
| `search` | `dataObjects:search` with a `vectorSearch` query filtered by scope |
| `count` | `dataObjects:query` filtered by scope, 1000 per page |
| `purge_scope` | `dataObjects:query` filtered by scope, then `dataObjects:batchDelete` |

Chunk text, metadata, provenance, scope, and chunk ID are stored **in** the Data Object alongside the vector — no companion store is needed:

| Data field | Holds |
|------------|-------|
| `scope` | The `Scope` key; every search, count, and purge filters on it inside the service |
| `chunkId` | The chunk ID, returned by search |
| `documentId`, `text`, `sourceUri`, `title` | The chunk's fields |
| `metadataJson` | The chunk's metadata as one JSON string |

Each key in `CollectionSpec::filterable` is a further string field, `m_<key>`, which `upsert` fills from the chunk's metadata. A `Filter` compiles to the service's `$eq`, `$in`, `$and`, and `$or`, plus `$ne` and `$nin` for a negated `Eq` or `In` on the document or chunk ID; `Exists`, other negations, and nesting deeper than two levels (the scope's `$and` counts as one) fail with `RagError::UnsupportedFilter` before any request. `count` and `delete_where` page through `dataObjects:query` with the same filter.

Agent Retrieval validates each Data Object against the collection's data schema and admits no undeclared field, so `create_collection` declares every field above, and metadata — whose keys vary — is one string. A Data Object ID is an RFC 1035 label, which a chunk ID such as `doc_0` is not, so each chunk is stored under `c` followed by the UUID v5 of its scope and chunk ID in hexadecimal. A collection created by an earlier release has no data schema: delete it, create it again, and re-ingest. The store runs in BYOE (bring-your-own-embeddings) mode: embeddings come from adk-rag's embedder pipeline, keeping behavior identical across backends. Scores are `DOT_PRODUCT` distances, which equal cosine similarity for normalized embeddings (higher is more relevant).

## Configuration

| Option | Effect |
|--------|--------|
| `with_collection_prefix("rag-")` | Prefixes every collection ID, isolating this store's collections |
| `with_existing_collections_only(true)` | `create_collection` validates the collection exists instead of creating it — for deployments that pre-provision with platform tooling |
| `with_endpoint(...)` | Custom API origin (loopback HTTP allowed for tests) |

Collection IDs are RFC 1035 labels. A collection name uses ASCII lowercase letters, digits, and underscores, starts with a letter (unless a collection prefix is set), ends with a letter or digit, and fits 63 characters with the prefix; each underscore becomes a hyphen, so `my_docs` is collection ID `my-docs`. Any other name — `my-docs`, `Docs`, `docs v2` — returns `RagError::VectorStoreError` instead of being rewritten, so two names never share a collection. A collection prefix uses ASCII lowercase letters, digits, and hyphens and starts with a letter.

## Beyond the trait

For callers holding the concrete `AgentRetrievalStore`, `hybrid_search` runs a semantic and a keyword query in one `dataObjects:batchSearch` call, both filtered by scope, and returns the reciprocal-rank-fusion ranked list:

```rust,no_run
# use adk_rag::agent_retrieval::{AgentRetrievalConfig, AgentRetrievalStore};
# use adk_rag::Scope;
# async fn run(store: AgentRetrievalStore, embedding: Vec<f32>) -> adk_rag::Result<()> {
let scope = Scope::single_tenant();
let results = store
    .hybrid_search("docs", &scope, &embedding, "quarterly revenue", 10, (0.6, 0.4))
    .await?;
# Ok(())
# }
```

## Platform notes

- **Regions:** Agent Retrieval is regional; supported regions include `us-central1`, `us-east4`, `us-west1`, `europe-north1`, `europe-west2`, `europe-west4`, `asia-east1`, `asia-northeast1`, and `asia-southeast1`.
- **CMEK:** standalone Agent Retrieval supports customer-managed encryption keys via the collection's `encryptionSpec` (immutable at creation). Pre-provision CMEK collections with platform tooling and use `with_existing_collections_only(true)`.
- **Vector Search 1.0** (index and index-endpoint resources) is not implemented.
- **Quotas:** batch writes are capped at 1000 objects per request and are atomic; the store chunks larger upserts automatically.

## Testing

The backend passes the shared `VectorStore` contract and property suite against an in-process mock of the `vectorsearch.googleapis.com` surface — which evaluates filters and pages queries as the REST reference describes — so it is behaviorally interchangeable with the other stores. The `#[ignore]` live test creates and deletes its own uniquely named collection, as the pgvector and Qdrant contract suites do on their servers:

```bash
GOOGLE_CLOUD_PROJECT=my-project GOOGLE_CLOUD_LOCATION=us-central1 \
  cargo test -p adk-rag --features agent-retrieval -- --ignored
```
