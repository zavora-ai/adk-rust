- **Crash recovery is audited** (`adk-runner`): `Runner::recover_unanswered_calls` records
  each call it answers in the runner's audit log, with the outcome its action ledger record
  supports (`succeeded`, `failed`, or `outcome_unknown`) and `cause: recovered`, under the
  runner's agent identity. A refused write under `AuditFailureMode::Block` leaves the calls
  unanswered for the next recovery.
