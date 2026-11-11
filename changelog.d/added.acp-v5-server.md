- **ACP server implements authentication, system prompts and the unstable session methods**
  (`adk-acp`): `AcpServerConfigBuilder::authenticator` takes an `AcpAuthenticator`, whose
  methods are advertised as `authMethods` and which answers `authenticate`, `logout` and
  `auth/status`. While it requires authentication, session requests get `auth_required`
  (`-32000`) until the client authenticates, and the server sends `auth/status_update` when
  the state changes. Client system prompts (`systemPrompt` on `session/new`, `load`, `resume`
  and `fork`) apply to every model request of the session's turns through a run invocation
  hook: `append` after the agent's own instructions, `override` instead of them.
  `AcpServerConfigBuilder::system_prompts` narrows this with `SystemPromptSupport`. The server
  also answers `session/set_title`, `session/status`, `session/archive`, `session/unarchive`
  and `session/options`, applies initial `configOptions`, filters `session/list` by archived
  state, reports titles there, and returns the turn's token `usage` in `PromptResponse`.
  `AcpServer::serve` serves one connection over any SDK transport, as `AcpServer::run`
  serves stdio.
