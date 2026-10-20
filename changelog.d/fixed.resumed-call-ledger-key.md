- **A resumed held call is ledgered under its original identity** (`adk-agent`): `LlmAgent`
  recorded a `NonIdempotent` call that a later run resumed under that run's invocation, so a
  call that ran in a run whose result was never saved ran again when the next run resumed it.
  A resumed call is now ledgered under `held_call_ledger_key` — the session, its original call
  ID and a digest of its request's fingerprint — which every resumption shares, and is answered
  `already_succeeded`, `already_failed` or `outcome_unknown` from its record. `adk-harness`
  looks stranded held calls up under the same key. Implements spec 3.1 §7.12, §9.2 I37.
