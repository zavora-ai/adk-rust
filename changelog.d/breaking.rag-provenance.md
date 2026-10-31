- **RAG chunks and search results carry `source_uri` and `title`** (`adk-rag`): every chunker
  dropped `Document::source_uri`, and no vector store kept it, so an answer could not cite the
  document a passage came from. `Document` gains `title`, and `Chunk` gains `source_uri` and
  `title`; the three built-in chunkers copy both onto each chunk, every vector store persists them
  as fields of their own and returns them with search results, and `RagTool` adds them to each
  result when present. Metadata is stored verbatim with no reserved keys. `Document` and `Chunk`
  are `#[non_exhaustive]`: build them with `Document::new(id, text)`, `Chunk::new(id,
  document_id, text)` or `Chunk::from_document(&document, id, text)` and the `with_*` builders.
  pgvector adds the columns in place on `create_collection`; LanceDB tables need re-creating and
  re-ingesting. Refs #1279.
