- **A confirmation that times out waits instead of being denied** (`adk-core`, `adk-agent`;
  spec 3.3 §7.3, §10 C2): when `RunConfig::tool_confirmation_timeout` passes and the run has
  an `approval_store` to read a later decision from, the call is recorded there (and
  submitted to the run's `approval_service`, if any) and the run ends with it held. A held
  call waiting in the store resumes only on a decision recorded for it: `LlmAgent`,
  `CodeActAgent` and the realtime runners do not ask the handler about it again, and a new
  message goes on to the model. `CodeActAgent` answers it in a turn of its own that cannot
  suspend, and keeps the waiting call's checkpoint. A decision from any source withdraws the
  call's open inbox requests and clears its pending entry; an inbox outcome being written at
  that moment is waited for and absorbed, and a call whose request does not settle within ten
  seconds fails with `approval.outcome_unsettled`. The service submits one request per held
  call, and `StoreApprovalService::decide` writes a resolved request's outcome before it runs
  the notifiers. A run without a store, including one with only an `approval_service`, still
  denies the call. `RunConfig::deny_on_confirmation_timeout` (builder
  `deny_on_confirmation_timeout(true)`) restores the denial. Harness agents, which always
  carry an approval store, now leave a call nobody answered in time waiting for a decision,
  and `Harness::decide` withdraws the call's inbox request before it records the person's
  decision, or refuses it when an approver decided the request first.
