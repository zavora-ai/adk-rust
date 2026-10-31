- **SurrealDB and Agent Retrieval collection names are validated** (`adk-rag`):
  `SurrealVectorStore` replaced every character other than a letter, digit, or `_` with `_`,
  and `AgentRetrievalStore` lowercased names, replaced other characters with `-`, and truncated
  them to 63, so distinct names such as `docs-v1` and `docs_v1` shared one collection. Both now
  return `RagError::VectorStoreError` for a name they would rewrite, as `PgVectorStore` does.
  `SurrealVectorStore` accepts ASCII letters, digits, and underscores and quotes table names, so
  keywords such as `select` are valid names. `AgentRetrievalStore` accepts ASCII lowercase
  letters, digits, and underscores and maps each `_` to `-`, so `my_docs` still reaches the
  collection 2.x named `my-docs`; a collection prefix uses lowercase letters, digits, and
  hyphens. The shared `VectorStore` contract checks that distinct names never share a
  collection.
- **Qdrant point IDs are derived from chunk IDs** (`adk-rag`): `QdrantVectorStore` used the chunk
  ID as the point ID, which Qdrant refuses unless it is a UUID, so chunks from the built-in
  chunkers (`doc_0`) could not be stored. Each chunk is now stored under the UUID v5 of its chunk
  ID with the chunk ID in the `chunk_id` payload field, which search returns; `delete` maps IDs
  the same way. Re-ingest a collection written by 2.x. The backend runs the shared contract
  against a Qdrant server in CI.
