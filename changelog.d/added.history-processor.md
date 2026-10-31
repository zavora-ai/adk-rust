- **History processors own an `LlmAgent`'s history between model calls** (`adk-agent`,
  `adk-core`): `LlmAgentBuilder::history_processor` adds an `adk_core::HistoryProcessor` that
  rewrites the agent's conversation history before each model request of a turn. Its output
  replaces the history, so a compaction runs once and carries over, while what toolsets,
  plugins, run hooks and before-model callbacks add still reaches each request once.
  `adk_core::history::balanced_content_cut` finds a cut over contents that keeps every function
  response with its call. Reported, with a first implementation, by
  [@kharkevich](https://github.com/kharkevich) (#1258).
