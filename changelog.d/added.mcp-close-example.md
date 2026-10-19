- **`mcp_close` example** (`examples/mcp_close`): `Runner::close` on an `LlmAgent` with MCP
  toolsets — a stdio server process exits (also on live Gemini), a Streamable HTTP session
  ends, composed toolsets close their members, a closed toolset does not reconnect, closing
  is bounded, and a shared `McpServerManager` keeps its servers until `shutdown`.
