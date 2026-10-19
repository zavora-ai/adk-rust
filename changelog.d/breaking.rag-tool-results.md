- **`RagTool` results carry no embedding vectors, and a `top_k` override is reranked**
  (`adk-rag`): `rag_search` returned each `SearchResult` whole, so every retrieved chunk's
  embedding went to the model (about 66 KB per chunk with Gemini's 3072 dimensions). Each
  result is now `{"id", "document_id", "text", "metadata", "score"}`. A `top_k` argument
  skipped the pipeline's reranker; it now runs through the new
  `RagPipeline::query_with_top_k`, which reranks and applies the similarity threshold like
  `query`. A missing `query` or a `top_k` that is not a positive integer fails with
  `tool.invalid_arguments`, so the model sees why.
