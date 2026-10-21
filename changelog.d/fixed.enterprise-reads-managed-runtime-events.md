- **`adk-enterprise` reads the events of a self-hosted managed runtime** (`adk-enterprise`): the
  client skipped every `status.idle` that carried a stop reason, because `adk-managed` tags
  it `reason` and the client read only `type`; it also skipped `error` events and
  `agent.mcp_tool_use` events with `tool_use_id`. Both tags, both error types and both ID
  names now parse; what the client writes is unchanged.
