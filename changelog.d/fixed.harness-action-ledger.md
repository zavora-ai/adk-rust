- **Harness calls run at most once across crashes and resumes** (`adk-harness`): the
  harness layer puts an action ledger on every run, so `bash`, `write_file` and
  `edit_file` are recorded before they execute and a re-issued call is answered from its
  record. A run that already carries a ledger, such as the runner's, keeps storing in it.
  A held call that a later run resumes is recorded under a key every resumption shares,
  so a call that ran in a process that stopped before saving its result is answered
  `already_succeeded` instead of running again, also when a live prompter approves it a
  second time. `Harness::send` and `Harness::resume` settle such a call first: answered
  from the ledger when it ran, held again for `pending_approvals` when its decision was
  consumed before it began. New `HarnessAgentBuilder::action_ledger` (default in memory)
  and `HarnessRuntime::action_ledger` (spec 3.1 §7.12, §9.2 I37).
