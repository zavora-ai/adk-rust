- **Action ledger** (`adk-core`, `adk-agent`, `adk-session`): an `ActionLedger` on
  `RunConfig::action_ledger` records each `NonIdempotent` call under its idempotency key
  before it executes and its outcome after. `LlmAgent` answers a replayed call from the
  ledger, and a call whose record was begun but never completed — a crash, timeout, or
  cancellation — is answered with `outcome_unknown` instead of executing again. A ledger
  that cannot be read or written fails the call closed. Ships `InMemoryActionLedger`
  (`adk-core`) and `SqliteActionLedger` (`adk-session`, `sqlite` feature), plus
  `outcome_unknown_response`, `is_outcome_unknown`, and `json_digest`.
