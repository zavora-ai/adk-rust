- **The harness's effort follows the mode on every model call** (`adk-harness`, `adk-cli`;
  spec 3.1 §7.23, §10 C4): `HarnessToolset` sets each request's `GenerateContentConfig::effort`
  to the `EffortPolicy` level of the mode it reads for that request, a member's toolset to
  `members`, and the distiller and MCP sampling models carry `distiller` and `sampling`.
  Approving a plan, `/mode` or `Harness::set_mode` changes the next call's effort on the same
  client; the effort never enters the request's contents, so the prompt prefix stays
  byte-stable (I56). `adk-rust` builds one client per session for the harness, its members
  and the distiller instead of one per role at a fixed level, and `/effort low|medium|high`
  sets every mode and role to that level from the next call instead of restarting the
  clients.
