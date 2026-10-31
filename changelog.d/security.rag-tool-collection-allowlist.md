- **`RagTool` searches only its configured collections** (`adk-rag`): `rag_search` searched
  any collection the model named, so a deployment with one collection per tenant let the model
  read another tenant's documents. `RagTool::new(pipeline, collection)` now offers no
  `collection` argument and refuses a call that names another collection with
  `tool.invalid_arguments`. `RagTool::with_allowed_collections` lists further collections the
  model may choose; the schema enumerates them and any other name is refused before the vector
  store is queried. `RagTool` is also read-only and concurrency-safe, so
  `DeclarativePolicy::allow_read_only()` admits it.
