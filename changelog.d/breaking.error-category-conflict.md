- **`ErrorCategory::Conflict`** (`adk-core`): a new category for a write that lost a race or
  used a stale version. It displays as `conflict`, maps to HTTP 409 (`http_status_code`, the
  `adk-server` problem responses), is not retryable by default, and is checked with
  `AdkError::is_conflict`. `ErrorCategory` is not `#[non_exhaustive]`, so an exhaustive `match`
  on it needs a `Conflict` arm. `lease.stale_epoch`, `run_store.stale_epoch`, and
  `governance.conflict` use it.
