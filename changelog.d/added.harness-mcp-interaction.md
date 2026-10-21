- **MCP interaction under the harness's rules** (`adk-harness`, feature `mcp`; spec 3.1
  §7.21): `HarnessAgentBuilder::mcp_servers` and `mcp_interaction`. A server's elicitation
  reaches the person as a prompt naming the server, the schema rendered as typed fields,
  the answer validated before it is returned; without a prompter it parks in the approval
  inbox as `mcp:<server>/elicitation` with `Edit` and `Reject`; without either it is
  declined. Sampling runs the harness model with no tools, `max_tokens` capped, the server's
  system prompt delimited as untrusted, metered under the run's budget and
  `SpendKey::org(app).with_agent("mcp:<server>")`. `mcp_list_resources` and
  `mcp_read_resource` (class `Read`) deliver resources inside an untrusted block; in
  `adk-rust`, `@server:uri` hands a resource to the model and `/mcp [resources SERVER]`
  lists servers and resources; `.mcp.json` servers start once the workspace is trusted.
  Audit records `harness.mcp.elicited`, `harness.mcp.sampled`, `harness.mcp.resource.read`.
  The umbrella forwards it as `harness-mcp`.
