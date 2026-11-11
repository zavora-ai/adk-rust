- **ACP MCP over ACP on both sides** (`adk-acp`): adk-acp enables the SDK's
  `unstable_mcp_over_acp` surface (`McpServer::Acp` declarations and `mcp/message`,
  carrying MCP `2026-07-28`). On the client, `AcpAgentConfig::tool_server` takes an
  `AcpToolServer`, a named set of ADK-Rust tools that run in the client application.
  `AcpConnection` declares each one, with a fresh `serverId`, on `session/new`, `load`,
  `resume` and `fork` once the agent advertises `mcpCapabilities.acp`. It answers the
  agent's `tools/list`, `tools/call` and `server/discover` over the same connection; a
  failing tool comes back to the agent as an `isError` result. `AcpToolServer` also
  implements the SDK's `McpService`, so it can be attached with `McpServer::new_service`
  and `SessionBuilder::with_mcp_server`. The server advertises `mcp.acp` and accepts
  `acp` MCP servers in session setup requests. It reaches their tools by sending
  `mcp/message` to the client, and maps tool results the way adk-tool's MCP toolset does.
