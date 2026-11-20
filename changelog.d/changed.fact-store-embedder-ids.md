- **`FactStore` embeddings carry their embedder's id** (`adk-harness`):
  `put_embedding` takes a `FactEmbedding { embedder, vector }` and `embeddings` returns
  one per fact. `WebSearchPath::Provider` reports `filter` and `filter_applied` instead of
  `allowlist` and `allowlist_supported`, `WebSearchPath::Governed` gains `filter`, and
  `harness.web.provider_enabled` records `searchDomains` instead of `allowlist`.
