- **Tool callback content answers the call it replaces** (`adk-agent`): content a
  `before_tool` or `after_tool` callback returns in place of a tool result becomes a function
  response with the call's id and tool name, instead of being sent as-is, which left a
  text-only answer without a response for the call and made OpenAI and Anthropic reject the
  next request. Text-only content is wrapped as `{"error": "<text>"}` from `before_tool`,
  since the tool did not run, or `{"result": "<text>"}` from `after_tool`.
