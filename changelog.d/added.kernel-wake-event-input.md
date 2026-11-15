- **A woken process reads the event that woke it** (`adk-kernel`, `adk-server`): a sleeper
  the `WakeRouter` wakes, and a run a `Trigger` spawns, start their turn with the wake's
  source, key, time and payload as user content, after the run's own input. Before, the
  event was recorded only on the `kernel.wake` run and the turn re-ran its fixed prompt.
  - `WakeReason` holds the reason, its payload cut to `WAKE_PAYLOAD_LIMIT` (4096 bytes of
    compact JSON) with a truncation marker; the `kernel.wake` run keeps the whole payload.
    The router records it on the process row (label `kernel.woke`), so it survives a
    kernel restart on every `ProcessStore` backend.
  - The payload is presented as delimited data in a user message, so
    `Agent::screen_user_input` and the input guardrails screen it; it never joins the
    agent's instruction.
  - `WorkerHooks::wake_input` (default `None`) is the hook a `DurableWorker` reads the
    reason through; `WakeReasonHooks` implements it for a worker composed without a
    `Kernel`. A session that already holds the wake is not told it again.
  - `Wake::At`, an expiry with `ExpiryAction::Run` and the file reconcile on start tell
    the turn the time or the changed path.
