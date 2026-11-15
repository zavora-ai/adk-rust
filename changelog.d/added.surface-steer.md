- **Steering from the surfaces, the control plane and chat** (`adk-surface`, `adk-kernel`): a
  message sent while a turn of its session runs reaches that turn through `Runner::steer`
  instead of waiting to become the next one.
  - `adk-surface`: `Command::Steer { session_id, text }` sends a message to the running turn
    of the caller's own session and answers `CommandOutcome::Steered { session_id, event_id }`
    when the turn took it, or `Queued`/`Sent` when it goes as the session's next message.
    `Source::steer` is the seam (`Steered`, `Steering`); `LocalSource` reaches the run in
    flight of its runner, harness or kernel, and `ControlSource` the kernel's steer route.
    `surface.steer` (`SURFACE_AUDIT_STEER`) records each steer with its outcome: `offered`,
    `delivered`, `returned` or `refused`.
  - `adk-kernel`: `Kernel::steer(user_id, session_id, text)` answers a `Steering` — the
    process, its lease epoch and the `SteerReceipt` — for a turn this replica runs, and `None`
    otherwise. `POST /kernel/sessions/{session}/steer` (`api::SteerRequest`,
    `api::SteerResponse`) and `ControlClient::steer` serve it to the person, recorded as
    `kernel.control` action `steer`. `ChannelHost::steer` lets the chat gateway hand a
    message to its thread's running turn (`InboundOutcome::Steered`); one the turn hands back
    starts the thread's next turn, once.
  - A turn another kernel replica runs is not steered: the message is handed back and goes as
    the session's next message.
