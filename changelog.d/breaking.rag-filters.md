- **RAG vector stores filter, count, and delete by a typed `Filter`** (`adk-rag`): no store could
  narrow a search to a tenant's or site's documents, delete by metadata, or remove a document,
  and re-ingesting a shorter document left its old chunks behind. `Filter` (`Eq`, `In`, `Exists`,
  `And`, `Or`, `!`) over metadata keys, `document_id`, `source_uri`, and the chunk ID compiles per
  backend to bound parameters or typed expressions and applies with the scope before the top `k`
  are chosen; a filter a backend cannot express, an empty `And`/`Or`/`In`, or an undeclared
  metadata key fails with the new `RagError::UnsupportedFilter` before any I/O. `VectorStore`
  gains `delete_where`, `count` takes an optional filter, `VectorQuery::with_filter` filters a
  search, and `CollectionSpec::with_filterable` declares the metadata keys a filter may name.
  `ScopedRag` gains `count_where`, `delete_where`, and `delete_document`, `QueryOptions` gains
  `with_filter`, and `RagPipeline::create_collection_with_filterable` declares keys. `ingest` now
  replaces a document's chunks. `LanceDBVectorStore::purge_deleted` physically removes deleted
  rows and old table versions. `RagTool` resolves each call's scope from its context
  (`with_scope_from`, `with_scope_per_user`), applies a server-side `with_policy_filter`, and
  offers the model a `filter` argument limited to `with_model_filterable` keys. Refs #1279.
