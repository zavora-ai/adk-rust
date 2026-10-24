- **A confirmation that times out waits instead of being denied** (`adk-core`; spec 3.3
  §7.3, §10 C2): when `RunConfig::tool_confirmation_timeout` passes and the run has an
  `approval_store` to read a later decision from, the call is recorded there (and submitted
  to the run's `approval_service`, if any) and the run ends with it held. Only a decision
  recorded for it resumes it; later runs do not ask the handler again, so a new message goes
  on to the model. A decision from any source cancels the call's pending entry and open
  inbox request, and the service submits one request per held call. A run without a store,
  including one with only an `approval_service`, still denies the call.
  `RunConfig::deny_on_confirmation_timeout` (builder `deny_on_confirmation_timeout(true)`)
  restores the denial. Harness agents, which always carry an approval store, now leave a
  call nobody answered in time waiting for a decision.
