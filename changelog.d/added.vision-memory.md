- **Visual memory** (`adk-vision`, feature `memory`; spec 3.7 §7.15): `VisualMemory` stores
  each image's embedding as an `adk-rag` chunk in any vector store — the caption as its text,
  the artifact as its `source_uri`, and digest, region, source, capture time and the storing
  run as metadata — and finds images again by text or by an example image, with day, kind
  and source filters. Each collection records its embedding space and refuses an embedder of
  another (`vision.memory.space_mismatch`). Captions go to `adk-memory` with a reference to
  their artifact, sources and labels to graph memory, and `erase_source` deletes a source's
  chunks, captions, graph entity and unshared artifacts with a `vision.memory.erased` record.
  `SiglipTextEmbedding` makes the embedder an `adk_rag::EmbeddingProvider`, and
  `memory::fixtures::WeekCorpus` generates scenario S16's week of frames and pages.
