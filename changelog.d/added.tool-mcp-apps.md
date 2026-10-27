- **MCP Apps on the client** (`adk-tool`): the `mcp-apps` feature declares the
  `io.modelcontextprotocol/ui` extension (`AdkClientHandler::with_apps`, and every server
  `McpServerManager` runs), reads each tool's `_meta.ui` (`McpToolset::ui_bindings`) and its
  `ui://` app with the sites its CSP declares (`McpToolset::read_app`, `McpAppResource`),
  who may call each tool (`McpToolset::tool_visibility`, `McpUiVisibility::of_tool`) and the
  name an agent knows a managed server's tool by (`McpServerManager::agent_tool_name`),
  leaves tools only an app may call out of the model's tools, and writes the `adk.mcp.app`
  record (`McpAppRecord`) when a tool with an app succeeds (spec 3.3 §6.1 `genui-apps`). See
  `docs/official_docs/tools/mcp-tools.md`.
- **An AG-UI extension point** (`adk-server`): `AgUiExtension`, set with
  `ServerBuilder::with_ag_ui_extension` or `RuntimeController::with_ag_ui_extension`, adds
  events to protocol-native `ag_ui` runs, decides which client-sent `activity` messages are
  kept, and turns a forwarded action into a requested call the run's agent
  (`AgUiAction::agent_name`) holds, so the run ends with a `tool_call` interrupt; its
  features join `ag_ui` in `GET /api/ui/capabilities`
  (spec 3.3 §6.1 `genui`, §10 S2).
