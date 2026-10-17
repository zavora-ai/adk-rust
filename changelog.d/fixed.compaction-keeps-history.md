- **Compaction summaries keep earlier summaries and tool results** (`adk-agent`):
  `LlmEventSummarizer` read only the text parts of each event, so a new summary dropped the
  previous summary and every function call and response in its window. With
  `RunConfig::history_max_events`, older turns then vanished from the model's context after the
  second compaction. The summarizer now reads the window as the model sees it: the latest
  earlier summary in place of the events it covers, then text, function calls and responses, and
  references to file, inline, and resource data. Thinking parts and streamed partial events stay
  out.
