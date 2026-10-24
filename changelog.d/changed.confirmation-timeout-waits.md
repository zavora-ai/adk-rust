- **A confirmation that times out waits instead of being denied** (`adk-core`; spec 3.3
  §7.3, §10 C2): when `RunConfig::tool_confirmation_timeout` passes and the run has an
  `approval_store` or `approval_service`, the call is recorded there and the run ends with it
  held, as if no handler had been asked; a later decision resumes it on the next run. A run
  with neither still denies the call. `RunConfig::deny_on_confirmation_timeout` (builder
  `deny_on_confirmation_timeout(true)`) restores the denial. Harness agents, which always
  carry an approval store, now leave a call nobody answered in time waiting for a decision.
