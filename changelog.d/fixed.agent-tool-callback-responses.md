- **Tool callback content answers the call it replaces** (`adk-agent`): content a
  `before_tool` or `after_tool` callback returned in place of a tool result was sent
  as-is, so text-only content left the function call without a response carrying its id
  and OpenAI and Anthropic rejected the next request. `LlmAgent` now sends a function
  response part with the call's id and tool name, and wraps text-only content as
  `{"error": "<text>"}` from `before_tool`, since the tool did not run, or
  `{"result": "<text>"}` from `after_tool`.
