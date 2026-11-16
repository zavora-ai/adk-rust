- **`memory-local` is no longer an `adk-cli` default feature** (`adk-cli`): a default
  `cargo install adk-cli` no longer downloads the ONNX Runtime binary or builds tokenizers'
  C code, so it builds offline. Without it, long-term memory for providers other than Gemini
  and OpenAI recalls by shared words, and `--embedder local` is refused with the reinstall
  hint. `cargo install adk-cli --features memory-local` restores the local embedder.
