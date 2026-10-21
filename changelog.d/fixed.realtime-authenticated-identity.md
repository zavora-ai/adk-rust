- **Voice sessions can carry the authenticated identity** (`adk-realtime`):
  `IntegratedRealtimeRunnerBuilder::request_context` runs every tool call of the session as the
  identity the server authenticated, so its tool contexts answer `is_authenticated()` with
  `true` and `user_scopes()` with the context's scopes, as an `adk-server` run does; before,
  the integration's tool context reported every voice user as unauthenticated.
  `RealtimeToolContext::with_request_context` sets it on a context built by hand. A context
  for a user other than the session identity's fails `build`.
