- **Every RAG vector store operation names a `Scope`** (`adk-rag`): chunks were keyed by chunk ID
  alone, so two tenants sharing a collection overwrote each other's chunks for the same document
  ID, and no store could search, count, or delete one tenant's chunks. `VectorStore::upsert`,
  `delete`, and `search` take a `Scope`, and the trait gains `count` and `purge_scope`; a chunk is
  identified by collection, scope, and chunk ID, and every backend applies the scope inside its
  search, before the top `k` are chosen. `create_collection` takes a `CollectionSpec` and `search`
  a `VectorQuery`. `RagPipeline::ingest`, `ingest_batch`, `query`, and `query_with_top_k` move to
  `ScopedRag` (from `RagPipeline::scoped`) as `ingest`, `ingest_batch`, `query`, and `query_with`
  (with `QueryOptions`), beside `count` and `purge_scope`. `RagTool` searches
  `Scope::single_tenant()` unless `with_scope` sets another. pgvector migrates tables in place,
  assigning existing rows to `Scope::single_tenant()`; LanceDB, Qdrant, SurrealDB, and Agent
  Retrieval collections need re-ingesting. Refs #1279.
- **Agent Retrieval stores chunks under derived IDs with a declared data schema** (`adk-rag`):
  `AgentRetrievalStore` used chunk IDs as Data Object IDs, which the service accepts only as
  RFC 1035 labels, so chunker IDs such as `doc_0` were refused, and it wrote fields no data schema
  declared, which the service rejects. Each chunk is now stored under `c` plus the UUID v5 of its
  scope and chunk ID, with the chunk ID in the `chunkId` field, and `create_collection` declares a
  data schema with metadata stored as the `metadataJson` string. Refs #1279.
