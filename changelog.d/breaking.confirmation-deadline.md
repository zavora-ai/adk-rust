- **Approval handlers can own their deadline** (`adk-core`):
  `RunConfig::tool_confirmation_timeout` is now `Option<Duration>`. Direct field
  assignments use `Some(duration)`; the duration-taking builder and five-minute
  default remain unchanged. `None` delegates the deadline to the handler, allowing
  a durable approval owner to arbitrate expiry against commit.
