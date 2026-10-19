- **`mcp_hardening` example** (`examples/mcp_hardening`): an `LlmAgent` with `McpToolset`
  and `McpServerManager`, run through a `Runner` against scripted MCP servers that page
  without end, cycle cursors, repeat or mangle tool names, hang, collide on prefixed names,
  or answer HTTP 500, asserting the discovery bounds, name checks, annotations, reconnect
  rules, cache invalidation, and tool filter, plus one live Gemini turn.
