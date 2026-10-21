- **Local posture** (`adk-cli`, `adk-rag`, `adk-harness`, `adk-rust`; spec 3.1 §5 H10, §7.15.2,
  §8.1, §8.4, §10 R1): `ollama` is in `adk-cli`'s default features and `--provider mistralrs
  --model <Hugging Face id or .gguf path>` runs mistral.rs in process behind the opt-in
  `mistralrs` feature (the umbrella forwards it as `mistralrs` and `cli-mistralrs`; a heavy
  build, in no tier). `adk-rag` gains `OnnxEmbeddingProvider` (feature `onnx`): a
  sentence-transformer ONNX model, `BAAI/bge-small-en-v1.5` by default, fetched once into
  the Hugging Face hub cache and run on the `ort` runtime `adk-audio` pins, so recall needs
  no network and no key; `adk-harness`'s `memory-embeddings-local` adds `RagEmbedder::local`
  and `RagEmbedder::onnx`, and the CLI uses it for long-term memory with a local provider.
  `examples/harness_agent` gains the `local_posture` scenario (`--features local`), which runs
  on Ollama when a model is reachable and is SKIPPED with the reason otherwise.
