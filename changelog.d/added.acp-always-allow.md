- **ACP "Always allow" creates a standing rule** (`adk-acp`, `adk-harness`):
  `AcpServerConfigBuilder::standing_permissions` takes a `StandingPermissions`, and a
  permission request for a call it may remember also offers `acp-allow-always` and
  `acp-reject-always`, worded with what the rule covers; choosing one decides the call and
  remembers the answer. An "always" answer to a request that offered none is a denial.
  `adk-harness`'s `acp-server` feature implements `StandingPermissions` for `HarnessAgent`:
  the answer becomes the narrowest standing rule the harness suggests, pinned to the tool's
  declaration and the session's person and recorded as `harness.rule.created` with surface
  `acp`, so the next identical call runs without asking; a payment, leaving plan mode, a
  memory write, a desktop action, a protected edit or an unsandboxed retry is never offered
  "always" (spec 3.3 §10 A1). See `docs/official_docs/acp/server.md`.
