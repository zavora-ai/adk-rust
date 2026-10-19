- **`AcpServerHandle::shutdown` lets running prompts finish** (`adk-acp`): shutdown dropped the
  ACP connection at once, so a prompt in progress never answered and `shutdown_timeout` had no
  effect, although `shutdown()` documented a graceful wait. Running prompts now keep the
  connection open and answer for up to `shutdown_timeout`; those still running then are
  cancelled, and the connection closes after the answers already given are written to
  stdout.
  `AcpSessionHandler::drain_sessions` waits for running prompts up to its timeout.
