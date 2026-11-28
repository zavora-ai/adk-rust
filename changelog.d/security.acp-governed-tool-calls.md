- **Tool calls on both ACP paths are governed** (`adk-acp`): each call an external ACP
  agent makes to an `AcpToolServer` runs through `adk_core::authorize_tool_call` and the
  action ledger before the tool executes, under `AcpToolServer::run_config` — its kill
  switches, consent gate, egress and tool policies, confirmation handler, approval store
  and audit trail. Every tool server has its own kill switch, `AcpToolServer::governance`.
  A frozen control answers the call `not run`; a call held for approval is not run in
  the request that asked for it, and is refused when no handler, approval store or
  approval service can decide it. Decisions keyed by call ID are dropped from the tool
  server's configuration, since the agent chooses each MCP request id, and the calls'
  acting principal comes from local configuration only. A tool's context exposes the run
  configuration, so an agent-as-tool delegate inherits it instead of running on
  `RunConfig::default()`. On the server side, every turn's `Runner` shares one kill
  switch, `AcpServerConfig::governance`, so an operator's freeze reaches ACP turns.
