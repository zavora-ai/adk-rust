- **MCP calls on one toolset run concurrently** (`adk-tool`): `McpToolset` and its tools
  held the connection lock across every request, so a slow `tools/call` blocked
  discovery and every other call on that server. Each request now holds the connection
  only for its own lifetime. Concurrent failures on one connection reconnect once, and
  the old connection closes after the requests still using it finish. The workspace
  lockfile moves to rmcp 3.5.1, whose Streamable HTTP transport sends concurrent
  requests in parallel.
