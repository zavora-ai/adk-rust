- **Compaction summaries keep earlier summaries and tool results** (`adk-core`, `adk-agent`,
  `adk-runner`): `LlmEventSummarizer` and `SummarisationCompaction` used only the text parts of
  each event, so a summary dropped every function call and response in its window, and
  `LlmEventSummarizer` also dropped the previous summary; with `RunConfig::history_max_events`,
  older turns vanished from the model's context after the second compaction. Both now render
  events with `adk_core::intra_compaction::render_part_for_summary`: text, function calls and
  responses, and references to file, inline and resource data. `LlmEventSummarizer` reads the
  latest earlier summary in place of the events it covers. Thinking parts and streamed partial
  events stay out.
- **Compaction counts tool payloads and keeps history a summary would not shrink**
  (`adk-core`, `adk-agent`, `adk-runner`): the runner's compaction token estimate uses
  `adk_core::intra_compaction::estimate_tokens`, which counts call arguments and responses
  instead of 20 characters per non-text part, and also counts compaction summaries. A summary
  that is empty or not smaller than what it replaces leaves the history as it is, and a
  summarizer model that stops for a reason other than `Stop` fails `LlmEventSummarizer` and
  `SummarisationCompaction` with `model.summary_incomplete`.
