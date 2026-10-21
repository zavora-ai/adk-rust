- **Elicitation scopes and per-server MCP handlers** (`adk-tool`; spec 3.1 §10 T1, T2):
  `ElicitationScope` and `ElicitationScopes` mirror `SamplingScopes` — `McpToolset`
  registers the scope of each in-flight tool call and the connection's handler answers an
  elicitation or a sampling request inside it, so a handler reads the (app, user, session,
  run id) of the call that triggered the request through `ElicitationScope::current()`;
  `McpToolset::with_elicitation_scope` sets the fallback. `McpServerManager::
  with_server_elicitation_handler` and `with_server_sampling_handler` set handlers for one
  server, preferred over the shared ones.
