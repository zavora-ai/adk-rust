- **Agent Retrieval collection names are validated** (`adk-rag`): `AgentRetrievalStore`
  lowercased names, replaced other characters with `-`, and truncated them to 63, so distinct
  names such as `Docs`, `docs`, and `docs_` shared one collection. It now returns
  `RagError::VectorStoreError` for a name it would rewrite, as `PgVectorStore` does. It accepts
  ASCII lowercase letters, digits, and underscores and maps each `_` to `-`, so `my_docs` still
  reaches the collection 2.x named `my-docs`; a collection prefix uses lowercase letters, digits,
  and hyphens. The shared `VectorStore` contract checks that distinct names never share a
  collection.
- **Qdrant point IDs are derived from chunk IDs** (`adk-rag`): `QdrantVectorStore` used the chunk
  ID as the point ID, which Qdrant refuses unless it is a UUID, so chunks from the built-in
  chunkers (`doc_0`) could not be stored. Each chunk is now stored under the UUID v5 of its chunk
  ID with the chunk ID in the `chunk_id` payload field, which search returns; `delete` maps IDs
  the same way. Re-ingest a collection written by 2.x. The backend runs the shared contract
  against a Qdrant server in CI.
