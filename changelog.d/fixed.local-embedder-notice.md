- **The first local-embedder download is announced** (`adk-cli`, `adk-harness`, `adk-rag`):
  `adk-rust`, `code`, `goal` and `up` print that they are fetching the ~130 MB
  `BAAI/bge-small-en-v1.5` model, and how long it took, instead of starting silently for
  minutes. `OnnxEmbeddingConfig::is_fetched` and `RagEmbedder::local_is_fetched` report
  whether the files are already on the machine.
