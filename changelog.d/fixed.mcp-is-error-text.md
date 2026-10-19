- **An MCP `isError` result reaches the model as the tool's response** (`adk-tool`): since
  tool error messages are redacted, an MCP server's `isError` text was replaced by
  `tool '<name>' failed: tool.internal (tool.legacy)`, so the model could not follow the
  server's guidance and correct its call. `McpToolset` now answers such a result with
  `{"error": "<text blocks joined by newlines>"}` (the `structuredContent` JSON when there is
  no text), cut to 8 KiB, for direct calls and for task results; the call counts as
  answered, so `on_tool_error` does not run for it. Transport, protocol, and task failures
  remain tool errors and keep their redaction.
