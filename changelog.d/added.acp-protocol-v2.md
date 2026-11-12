- **Draft ACP protocol v2 behind `unstable-protocol-v2`** (`adk-acp`): the new opt-in
  feature forwards the SDK's `unstable_protocol_v2`. The server serves both versions
  through `Agent.protocol_router()`: a v1 client sees the v1 server unchanged, and a v2
  client gets `session/new`, `list`, `resume` (with replay from the start), `close`,
  `delete`, `set_config_option`, `prompt` and `cancel` over the same session handler. A
  v2 prompt is answered when its user message is inserted, with the message id; prompts
  sent during a turn are held in order; and each turn ends with an `idle` state update
  carrying the stop reason and token usage. `tool_call/cancel` and `terminal/cancel` get
  `-32602`. A server with an authenticator stays v1-only until v2 `auth/login` is
  implemented. `AcpConnection::connect` connects through `Client.protocol_connector()`,
  preferring v2 and falling back to v1. Against a v1-only agent process the fallback
  starts the process a second time, because the v1 initialize advertises capabilities the
  v2 request cannot carry. `AcpConnection::connect_with_factory` does the same over any
  transport the caller can reopen; `connect_to` keeps speaking v1. `agent_info()` reports
  the negotiated version, with a v2 agent's capabilities in their v1 form, and
  `SUPPORTED_PROTOCOL_VERSIONS` includes v2. `AcpConnection::sdk_connection` now returns an
  `Option` (it is `None` on a v2 connection), and `sdk_connection_v2` gives the v2
  connection. MCP over ACP is served on v1 only: a client with `tool_servers` connects
  over v1, and the v2 server refuses `acp` MCP servers, which v2 cannot declare. Without
  the feature, both sides behave as before.
