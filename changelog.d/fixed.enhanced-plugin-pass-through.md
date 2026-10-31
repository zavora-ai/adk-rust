- **A result enhanced plugins pass through keeps its parts** (`adk-agent`): when every
  `after_tool_call` plugin returns the result unchanged, `LlmAgent` keeps the function response
  as it was, with its inline and file data and its annotations, instead of rebuilding it from
  the JSON payload.
