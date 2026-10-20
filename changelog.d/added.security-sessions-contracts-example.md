- **`security_sessions_contracts` example** (`examples/security_sessions_contracts`):
  `LlmAgent` and `Runner` with the `adk-payments` toolset, `SsoAccessControl` with a real
  `JwtValidator` over a local ES256 issuer, and the in-memory, file, encrypted, SQLite, and
  Redis backends, asserting that an escalated payment held for review is paid once after an
  approval store decision, that an SSO grant the audit sink cannot record is refused, and that
  missing artifacts, traversal names, and invalid session identifiers fail with typed codes,
  against a scripted OpenAI-compatible server, a local Redis, and live OpenAI.
