- **`approval_scoping` example** (`examples/approval_scoping`): `LlmAgent`s in a
  `SequentialAgent` over the SQLite session service and approval store, a scripted
  OpenAI-compatible server, two in-process MCP servers behind real `McpToolset`s, and a live
  OpenAI scenario, asserting that an approval for one agent's held call does not authorize
  another agent's same call, that the approved call runs once, that a held MCP call does not
  resume on another server or a redefined tool, and that an agent's own fingerprint stays valid
  across runs.
