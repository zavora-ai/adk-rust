- **`AcpSessionHandler` session setup takes the client connection** (`adk-acp`, `server`
  feature): `create_session`, `resume_session` and `fork_session` take a
  `ConnectionTo<Client>` after the request cancellation, as `load_session` already did.
  The session's `acp` MCP servers are reached through it. `AcpServerConfig` gains a
  `providers` field.
