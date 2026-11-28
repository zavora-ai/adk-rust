- **Realtime runners have no ungoverned mode** (`adk-realtime`): `RealtimeRunner` and
  `IntegratedRealtimeRunner` built without `tool_governance` ran every tool call outside
  `adk_core::authorize_tool_call`, with no kill switch, confirmation or acting identity. They
  now govern it under `RunConfig::default()` with a kill switch of their own (`governance()` on
  the runner, `governance(control)` on the builder to share one), so a freeze stops the next
  call of an open session. A call that needs confirmation and that nothing can decide — no
  confirmation handler, approval, approval store or approval service, and for
  `IntegratedRealtimeRunner` no session to hold it in — is refused with a reason the model can
  speak, and a `warn` event names the tool. Calls nothing denies still run; restrict them with a
  `ToolPolicy` in `tool_governance`. `IntegratedRealtimeRunner::governance()`, which returned
  `Option<&RunConfig>` on pre-release `main`, returns the `GovernanceControl`; read the config
  with the new `run_config()`, also on `RealtimeRunner`.
