- **ACP client covers the v1 protocol surface** (`adk-acp`): `AcpConnection` opens one
  agent connection and exposes `new_session`, `load_session` (its replay is read with
  `AcpSession::pending_updates`), `resume_session`, `fork_session`, `list_sessions`,
  `delete_session`, `archive_session`, `unarchive_session`, `session_status`,
  `session_options`, `authenticate`, `logout`, `auth_status` and `agent_info`, which reports
  the negotiated protocol version, the agent's identity, capabilities and auth methods.
  `AcpConnection::connect_to` runs over any SDK transport. `AcpSession` adds
  `prompt_content`, `prompt_blocks` (every update of the turn as an `OutputChunk`),
  `set_mode`, `set_config_option`, `set_title`, `status`, `fork`, `archive`, and the
  session's modes, config options, commands and title. `AcpAgentConfig` adds
  `append_system_prompt`, `override_system_prompt`, `additional_directory`,
  `initial_config_option`, `auth_method`, `elicitation` and `client_info`, each checked
  against the agent's capabilities before it is sent. The client advertises the updates it
  surfaces (config options, notices, compactions, plan operations, subagents), answers
  `elicitation/create` through an `AcpElicitation` (`DeclineElicitation` declines), records
  `auth/status_update`, and routes a subagent's updates to the session that announced it.
