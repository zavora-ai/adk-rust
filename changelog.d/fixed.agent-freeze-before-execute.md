- **No tool call starts after a freeze is observed** (`adk-core`, `adk-agent`, `adk-runner`):
  the kill switch was checked only when a call was authorized, so a call whose action-ledger
  write outlasted a freeze still started. `LlmAgent` and `CodeActAgent` now check it again
  between the ledger write and `execute()`, and before each retry. A call refused there is
  answered `not run`, as at authorization, and its begun ledger record is completed as
  `ActionOutcome::Failed` with that text instead of reading as an unknown outcome: a replay is
  answered `already_failed` and run recovery audits it as `NotRun`. `adk_core::NOT_RUN_PREFIX`,
  `not_run_text` and `is_not_run` name that text. Spec 3.1 I20, spec 3.2 K-I8.
