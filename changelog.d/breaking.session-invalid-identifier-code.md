- **Invalid session identifiers fail with `session.invalid_identifier`** (`adk-core`,
  `adk-session`): converting an `IdentityError` into `AdkError` used the generic
  `config.legacy` code in the `Server` component, so every session backend, the encrypted
  wrapper, and `Runner::run_str` reported an empty, oversized, or NUL-containing app name,
  user ID, or session ID under a code that named no failure. They now return
  `session.invalid_identifier` (`ErrorComponent::Session`, `ErrorCategory::InvalidInput`,
  HTTP 400), with a message naming the rule. `VertexAiSessionService` runs this check before
  its own, so an empty identifier no longer reports `session.vertex.invalid_input`.
  `AdkError::is_config()` is false for these errors.
