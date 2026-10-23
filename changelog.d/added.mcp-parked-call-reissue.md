- **Parked MCP calls survive a restart by re-issue** (`adk-tool`): `ParkedMcpCall` is the
  serializable form of a call waiting for input — server ID, tool name, arguments, call ID,
  the server's `requestState`, the input requests and their digest, the input rounds taken,
  and a `kind` of `mrtr`, `task { taskId }` or `legacy`. `McpToolset::call_tool_resumable`
  ends a call at its first question with `McpCallOutcome::Parked`, a handler answering in
  process reads the call with `ParkedMcpCall::current()`, and `McpToolset::reissue` (or
  `McpServerManager::reissue_parked_call`) resumes it: a new `tools/call` with
  `inputResponses` and the stored `requestState`, `tasks/get` answered through
  `tasks/update` for a task, and for a legacy call a second run only when the tool is
  retry-safe (otherwise `tool.mcp.not_run`). A re-issue answered with new questions parks
  again under `max_input_rounds`; in-process answering stays the fast path. Spec 3.2 §10 T2.
