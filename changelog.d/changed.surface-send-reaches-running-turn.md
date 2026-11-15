- **`Command::Send` reaches a running turn** (`adk-surface`): a message sent while a turn of
  its session runs still answers `Queued` at once, and the engine now offers it to that turn.
  The agent takes it after its current tool step, the session holds it as a user event with a
  steering marker, and it leaves the queue; a turn that ends first leaves it queued, and it
  goes as the next message, once. `SurfaceEngine::unqueue_last` returns `None` while the
  running turn holds the newest message. `Command::Queue` is never offered to a running turn.
  An input guardrail that blocks the message refuses the `Send` itself.
- **A chat message reaches its thread's running turn** (`adk-kernel`): `ChannelGateway::receive`
  answers `InboundOutcome::Steered` for a paired account's message that arrives while its
  thread's turn runs on the same kernel replica, instead of queueing a second turn behind it.
