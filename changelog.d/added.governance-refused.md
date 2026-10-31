- **`GovernanceControl::refused` and `RunConfig::governance_refused`** (`adk-core`):
  futures that resolve with the kill switch's error once a control starts refusing
  work, woken by freezes and mirror reads instead of polling.
