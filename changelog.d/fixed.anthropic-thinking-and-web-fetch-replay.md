- **Thinking and web fetch turns replay unchanged** (`adk-model`, `adk-anthropic`):
  signed `thinking` blocks, including the empty ones returned under `display: "omitted"`,
  are kept and sent back as thinking blocks instead of being dropped or turned into
  assistant text, and `redacted_thinking` blocks are kept through native history. Thinking
  without a signature is no longer sent. Web fetch results keep the document's
  `"type": "document"` and the error's `"type": "web_fetch_tool_result_error"` and code,
  so a replayed web fetch turn is no longer rejected with HTTP 400, and server tool results
  of every kind convert back from ADK parts.
