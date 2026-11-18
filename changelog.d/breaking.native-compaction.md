- **Native compaction changes compaction types** (`adk-core`, `adk-anthropic`):
  `HistoryCompactionRequest` is `#[non_exhaustive]` and gains `tools` and `config`; build it with
  `HistoryCompactionRequest::new(history, trigger)` and `with_preamble`,
  `with_reported_prompt_tokens`, `with_tools` and `with_config`. `IntraCompactionConfig` gains
  `native` (default `true`): on a model that compacts natively, such as Claude Sonnet 5.5 on the
  Claude API, the provider now writes the summary; `with_native(false)` keeps the runner's
  summarizer. `adk-anthropic`'s `ContentBlock` gains `Compaction`, `StopReason` gains
  `Compaction`, and `MessageCreateParams` gains `compaction`. An `Llm` wrapper forwards
  `native_compaction()`.
