- **Scenario S7 reads a run's spend across midnight UTC** (`examples/vision`): S7 and the
  desk's vision spend read the ledger over its lifetime instead of the current UTC day, so a
  run crossing midnight no longer splits its spend; S7 now crosses midnight on purpose.
