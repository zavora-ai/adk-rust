- **Realtime tool calls are governed** (`adk-realtime`): `RealtimeAgent`, `RealtimeRunner`,
  and `IntegratedRealtimeRunner` dispatch every tool call through
  `adk_core::authorize_tool_call` — the kill switch, the `ToolPolicy`, and confirmation.
  `RealtimeAgent` reads them from the run's `RunConfig` and also runs the runner's plugin
  agent and tool callbacks; the runners take them from the new `tool_governance` builder
  methods. Each runtime gains `tool_timeout` (default five minutes).
- **Governance admin endpoints** (`adk-server`): `ServerConfig::governance` is shared by every
  runner the server builds, and `ServerBuilder::enable_governance_endpoints` mounts
  `POST /api/admin/freeze`, `POST /api/admin/unfreeze`, and `GET /api/admin/governance`
  behind the auth middleware. A freeze also pauses background-run and cron scheduling
  through the new `BackgroundRunner::pause` and `CronJobStore::pause_scheduling`.
