- **`ErrorCategory` gains `ResourceExhausted` and `Conflict`** (`adk-core`): neither is
  retryable by default. `ResourceExhausted` marks a used-up run, team, or spend budget,
  displays as `resource_exhausted`, and maps to HTTP 429. `Conflict` marks a write that lost
  a race or used a stale version, displays as `conflict`, maps to HTTP 409
  (`http_status_code`, the `adk-server` problem responses), and is checked with
  `AdkError::is_conflict`; `lease.stale_epoch`, `run_store.stale_epoch`,
  `governance.conflict`, and `approval.version_conflict` use it. `ErrorCategory` is not
  `#[non_exhaustive]`, so an exhaustive `match` on it needs both arms.
