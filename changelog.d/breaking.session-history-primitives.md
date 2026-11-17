- **Session rewind is built on history primitives** (`adk-session`): `rewind` and
  `rewind_steps` are provided methods over `rewrite_history`, and a `SessionService` outside the
  crate implements `history`, `rewrite_history`, and `import_session` instead of `rewind`.
  `session.history.unsupported` replaces `session.rewind.unsupported`. A rewind that would keep
  a tool call without its response fails with `session.history.splits_tool_call`, and a rewind
  of a session holding a call for a decision fails with `session.history.held_call`.
  PostgreSQL migration 3 adds `sessions.initial_state`; the other backends record the creation
  state as a field of the session record.
