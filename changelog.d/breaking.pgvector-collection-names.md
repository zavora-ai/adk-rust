- **pgvector collection names are validated** (`adk-rag`): `PgVectorStore` accepts
  collection names of 1 to 59 ASCII lowercase letters, digits and underscores and
  returns `RagError::VectorStoreError` for any other name. 2.x rewrote invalid
  characters to `_`, so distinct names such as `docs-v1` and `docs_v1` shared the
  table `rag_docs_v1`. Use the existing table's name without the `rag_` prefix to
  keep reading a collection.
- **`PgVectorStore::new` returns `adk_rag::Result`** (`adk-rag`): a connection
  failure is `RagError::VectorStoreError` instead of `sqlx::Error`.
