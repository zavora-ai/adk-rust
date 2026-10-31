- **An invalid LanceDB collection name is an error, not a panic** (`adk-rag`): lancedb panics
  when it creates or opens a table whose name breaks its rule, so `LanceDBVectorStore` panicked
  on a name such as `docs v1`. The store now checks the rule first — ASCII letters, digits,
  underscores, hyphens, and periods — and returns `RagError::VectorStoreError`.
