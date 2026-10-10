- **`ConnectionRefresher` and `SimpleClient` send requests concurrently** (`adk-tool`):
  both held their connection mutex across every request, so a slow `tools/call` blocked
  discovery and every other call. Each request now holds only its own clone of the
  connection, concurrent failures reconnect once, and `ConnectionRefresher::call_tool`
  resends a call rejected with HTTP 401 after reconnecting, as `McpToolset` does.
