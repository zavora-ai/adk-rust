- **Steering a running agent** (`adk-core`, `adk-runner`, `adk-agent`): `Runner::steer(identity,
  content)` sends a user message to the run in flight for one exact identity. The runner validates
  it (role `user`, text and attachments only) and screens it like the run's own message —
  plugins' `on_user_message`, then every agent's input guardrails — and queues it on that run's
  `SteeringInbox`, reachable from `InvocationContext::steering_inbox`. `LlmAgent` and
  `CodeActAgent` take the queue after a tool step's results are in the history and before the
  next model request, and yield each message as a user-authored event marked with
  `EventActions::steering` (`SteeringMarker`: `steerId`, `sequence`, `sender`), which the runner
  saves, so the stream, plugins, the session, later agents and later turns see it. The returned
  `SteerReceipt` resolves to `SteerOutcome::Delivered { event_id }`, or to
  `SteerOutcome::Returned(content)`, the message as sent, when the run ends before a safe point
  takes it. Workflow members and transfer targets share the run's queue; `ParallelAgent`
  branches take none.
- **`Content` implements `PartialEq`** (`adk-core`).
