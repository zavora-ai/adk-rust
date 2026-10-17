- **History never sends a function response without its call** (`adk-core`, `adk-runner`):
  the history projection drops a response whose call is not retained — cut by a
  `history_max_events` window or a compaction — and a second response to a call already
  answered; Anthropic, OpenAI and Gemini reject both. Intra-invocation compaction, session
  compaction, `TruncationCompaction` and `SummarisationCompaction` cut only where no kept
  response answers a dropped call (`adk_core::history::balanced_cut`), on top of the held-call
  limit.
