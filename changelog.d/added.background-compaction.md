- **Background compaction** (`adk-core`, `adk-runner`): `IntraCompactionConfig::in_background`
  writes the auto-compaction summary off the critical path. Once a request passes
  `background_fraction` of the threshold, the runner starts the summary and lets the model call go
  ahead, then swaps it in before a later call — in the same run or the session's next turn — when
  the history still starts with the entries it covers; a request that reaches the threshold waits
  for it. No background compaction starts while a call is held for confirmation or a kill switch is
  frozen, and the summary call counts against the run's budget and spend ledger. Off by default.
