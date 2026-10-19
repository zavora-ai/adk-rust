- **A summary whose stream fails ends with `model.summary_incomplete`** (`adk-agent`,
  `adk-runner`): `LlmEventSummarizer` and `SummarisationCompaction` document that code for a
  stream error but returned the stream's own error. The error is now
  `model.summary_incomplete` with the stream error's category, retry hint, and details, its
  code in `details.metadata["cause_code"]`, and the stream error as its `source()`.
