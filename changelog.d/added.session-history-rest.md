- **Governed session fork, rewind, and edit over REST and the CLI** (`adk-runner`,
  `adk-server`, `adk-cli`, `adk-core`): `Runner::history_control()` returns a `HistoryControl`
  that refuses to fork, rewind, or edit a session while a turn runs on it
  (`session.history.run_active`, across every runner sharing a `SessionActivity` and the durable
  run store) and records a decision and an outcome per operation under the new
  `AuditRecordKind::SessionHistory`. `RewindTarget::BeforeInvocation` undoes a turn. `adk-server`
  serves `GET …/sessions/{id}/history`, `POST …/fork`, `POST …/rewind`, and
  `POST …/history/edit` behind the session routes' authentication, and shares
  `ServerConfig::session_activity` with every runner it builds. `adk-rust sessions fork ID
  [--turns N | --at EVENT_ID] [--as NEW_ID]` forks a harness session.
