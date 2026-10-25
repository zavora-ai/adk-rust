- **A provider search streams before the answer it informs** (`adk-model`, `adk-cli`): the
  Anthropic stream held `server_tool_use` blocks and their results until the complete message,
  so the console printed a `[server-tool-call]` line after the answer the search produced. Each
  `server_tool_use` block and server tool result now streams as a partial response at its
  `content_block_stop`, and the console prints a server tool call or result once, at its first
  chunk, matched to the complete message's copy by its id.
