- **Replicated kill switch** (`adk-core`): `GovernanceControl::mirrored` mirrors a
  `GovernanceStateStore` shared by every replica (`GovernanceMirrorConfig`: poll interval,
  `max_staleness`, fail-open, scopes, and an optional `GovernanceWatch` change hook);
  `spawn_mirror` and `refresh` keep it current, and `freeze_replicated` /
  `unfreeze_replicated` write the store by compare-and-swap. Past `max_staleness` without a
  successful read, a fail-closed control refuses work with `governance.stale`
  (`GOVERNANCE_STALE_CODE`, retryable); `GOVERNANCE_FROZEN_CODE` names the frozen code.
  The server's freeze and unfreeze endpoints write the store and log the caller, a freeze
  requests cancellation of running background runs, and the status reports `mirrored` and
  `stale`.
