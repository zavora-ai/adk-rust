- **`Runner::close`** (`adk-runner`, `adk-core`, `adk-tool`): closes a runner's agents and
  plugins when the application shuts down. It calls the new `Agent::close` on the root agent,
  whose default closes each sub-agent and which `LlmAgent` and `CodeActAgent` use to close
  their toolsets through the new `Toolset::close` hook (default: nothing to close), then
  closes the plugins. `Runner::allowed_transfer_targets(root, source)` returns the agents
  `source` may transfer to.
- **`McpToolset` closes its server** (`adk-tool`): `McpToolset::close` cancels the remote
  tasks the toolset started and its rmcp session, then waits until a stdio server's process
  has exited or a Streamable HTTP session is ended with `DELETE`, bounded by
  `McpToolset::with_close_timeout` (5 seconds by default); closing again is harmless. A closed
  toolset, its clones and its tools fail later requests with `tool.mcp.closed` and do not
  reconnect through a connection factory. `FilteredToolset`, `MergedToolset` and
  `PrefixedToolset` close their inner toolsets; closing an `McpServerManager` as a toolset
  leaves its servers running.
- **`agent_lifecycle` example** (`examples/agent_lifecycle`): transfer restrictions on tool
  transfers, literal braces in instructions, after-agent and after-run exits, a model-fallback
  plugin, per-invocation retry-reflect budgets, `output_key`, parallel escalation, `on_event`
  coverage and `Runner::close`, against a scripted OpenAI-compatible upstream and live Gemini.
