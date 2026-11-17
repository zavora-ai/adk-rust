- **Agents served over MCP** (`adk-server`, `adk-tool`): `adk_server::mcp::McpAgentHost`
  (feature `mcp`, experimental) publishes agents as MCP tools over stdio and Streamable HTTP.
  Each call is a durable run; a client that declares the tasks extension (SEP-2663) follows it
  as a task through `tasks/get`, `tasks/update` and `tasks/cancel`, and the task survives a
  server restart. A held tool call is asked as a form elicitation keyed to the call's
  fingerprint, and the answer resumes it through the approval service or store; a stale answer
  is refused. Clients without tasks are asked through `input_required` (MCP `2026-07-28`) or
  `elicitation/create`. `RequestContextCallers` resolves callers with the server's
  `RequestContextExtractor`. In `adk-tool`, `McpServer` gains `McpTaskHost`, `local_caller`, a
  stdio transport, and `Task` and `InputRequired` executor outcomes; `McpHttpClientBuilder`
  gains `with_task_support`.
