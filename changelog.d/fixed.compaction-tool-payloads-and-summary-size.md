- **Compaction counts tool payloads and keeps history a summary would not shrink**
  (`adk-core`, `adk-agent`, `adk-runner`): context compaction estimates tokens with
  `adk_core::intra_compaction::estimate_tokens`, which counts call arguments, responses and
  compaction summaries, and `SummarisationCompaction` renders events with the summarizer's
  renderer (`adk_core::intra_compaction::render_part_for_summary`), so tool results reach the
  summary. `LlmEventSummarizer` and `SummarisationCompaction` fail with
  `model.summary_incomplete` on a stream error or a finish reason other than `Stop`, and a
  summary that is empty or not smaller than what it replaces leaves the history as it is.
