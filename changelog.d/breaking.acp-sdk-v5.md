- **`adk-acp` speaks ACP through `agent-client-protocol` 5.2** (`adk-acp`):
  the SDK moves from 3.3 to `~5.2` (there is no 4.x), so the re-exported
  `adk_acp::agent_client_protocol` follows the SDK's v5 layout. Protocol v1 messages
  moved from `agent_client_protocol::schema::v1` to `agent_client_protocol::v1::schema`,
  `Client.builder()` / `Agent.builder()` are `Client.v1()` / `Agent.v1()`, and v1 callbacks
  take `v1::ConnectionTo` and `v1::Responder`. Both sides now let the SDK's v1 guard
  negotiate the protocol version: the server answers `initialize` with the negotiated
  version instead of a constant, and the client requests the highest version in
  `SUPPORTED_PROTOCOL_VERSIONS` and rejects an agent that selects another. Other changes:
  - `AcpAgentConfig`, `PromptResult` and `OutputChunk` are `#[non_exhaustive]`.
    `AcpAgentConfig` gains `additional_directories`, `system_prompt`,
    `initial_config_options`, `auth_method`, `elicitation` and `client_info`. `PromptResult`
    gains `stop_reason` and `usage`. `OutputChunk::Done` became
    `Done { stop_reason, usage }`, and `OutputChunk` gains variants for plans, notices,
    compactions, mode, config, command and session-info changes, user text and subagents.
  - `AcpSession::start` returns once the agent has created the session, so a refused
    handshake or `session/new` fails `start` instead of the first prompt.
  - `AcpError` gains `Unsupported`, returned when the agent did not advertise what a request
    needs. `AcpServerError` gains `AuthRequired` and `Unsupported`.
  - `AcpSessionHandler::resume_session`, `load_session` and `fork_session` take the
    `ResumeSessionRequest`, `LoadSessionRequest` and `ForkSessionRequest`, and `handle_prompt`
    returns the full `PromptResponse`, with the turn's token `usage`.
  - A request naming a missing session gets ACP's `session_not_found` error (`-32002` with
    `{"sessionId": …}`) instead of `-32602`.

  Migration: change `agent_client_protocol::schema::v1::X` to
  `agent_client_protocol::v1::schema::X`, `Client.builder()` to `Client.v1()` and
  `Agent.builder()` to `Agent.v1()`. Match `OutputChunk::Done { .. }` and add a wildcard arm
  to `OutputChunk` matches. Build `AcpAgentConfig` with `AcpAgentConfig::new` and its
  builder methods. Pass the request types to the `AcpSessionHandler` lifecycle methods.
  Treat `-32002` as "session not found". See the SDK's v5 migration guide for code that
  uses the SDK directly.
