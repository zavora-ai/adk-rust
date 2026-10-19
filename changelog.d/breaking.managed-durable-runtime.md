- **Managed custom tool calls end the turn** (`adk-managed`): a custom tool call emits
  `agent.custom_tool_use` and a `status.idle` with `requires_action`; the turn continues in
  a new `status.running` after `user.custom_tool_result`. A held confirmation reports
  `agent.tool_use` and ends the turn the same way. `send_event` refuses a `user.message`
  while answers are pending, and an answer to an ID the session is not waiting on.
  - Removed: `ToolParkingLot` and the `parking` module; `SessionLoop::new` and
    `SessionLoop::with_pause_controls` no longer take a parking lot.
  - `ManagedCustomTool::execute` answers with a confirmation-pending response and ends the
    agent's turn instead of returning `pending_client_execution`.
  - `ManagedSessionState` gains `record`, `pending`, and `durable_run_id`, so struct
    literals must set them; `ManagedSessionState::new(events, run_state)` sets the defaults.
  - `DefaultModelResolver` is no longer a unit struct; build it with
    `DefaultModelResolver::new()` or `DefaultModelResolver::default()`.
