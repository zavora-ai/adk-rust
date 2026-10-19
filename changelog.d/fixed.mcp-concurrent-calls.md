- **MCP requests on one connection run concurrently** (`adk-tool`): `McpToolset` and its
  tools, `ConnectionRefresher`, and `SimpleClient` held a connection mutex across every
  request, so a slow `tools/call` blocked discovery and every other call on that server.
  Each request now holds only its own clone of the connection. Concurrent failures on one
  connection reconnect once, the old connection closes after the requests still using it
  finish, and `ConnectionRefresher::call_tool` resends a call rejected with HTTP 401 after
  reconnecting, as `McpToolset` does. Streamable HTTP requests run in parallel with rmcp 3.2
  or later; the workspace lockfile moves from rmcp 3.1.4 to 3.5.1.
