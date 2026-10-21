- **A resumed held call keeps one idempotency key** (`adk-agent`): the
  `ToolContext::idempotency_key()` of a held call that a later run resumes is the call's
  `held_call_ledger_key` — its session, call ID, and fingerprint — in every run that resumes
  it, instead of a new key naming each resuming run's invocation. A tool that forwards the
  key downstream, such as a payment, sends one key for one call.
