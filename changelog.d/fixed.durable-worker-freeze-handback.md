- **A freeze no longer fails a run claimed across it** (`adk-server`): a claim already in
  flight when the governance kill switch froze could return a run after the freeze, and the
  runner then failed that run at its start, so queued work was lost. `DurableWorker` checks its
  control again once a claim returns, and hands a run the control refuses before its first
  model or tool call back unstarted: the run is `Waiting`, claimable at once, and the claim
  that resumes it after the freeze counts no further attempt.
