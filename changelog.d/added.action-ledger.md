- **Action ledger** (`adk-core`, `adk-agent`, `adk-runner`, `adk-session`): an `ActionLedger`
  on `RunConfig::action_ledger` or `Runner::builder().action_ledger(...)` records each
  `NonIdempotent` call under its idempotency key after the governed path authorizes it and
  before it executes, and its outcome after. `LlmAgent` and `CodeActAgent` answer a replayed
  call from the ledger (`already_succeeded`, `already_failed`), and a call whose record was
  begun but never completed — a crash, timeout, or cancellation — with `outcome_unknown`,
  instead of executing it again; `CodeActAgent` raises the answer into the script. A ledger
  that cannot be read or written fails the call closed. Ships `InMemoryActionLedger`
  (`adk-core`) and `SqliteActionLedger` (`adk-session`, `sqlite` feature), plus
  `outcome_unknown_response`, `is_outcome_unknown`, `OUTCOME_UNKNOWN_STATUS`, and
  `json_digest`.
