- **`CronState` is `#[non_exhaustive]`** (`adk-server`): it gains `jitter` and `audit`. Build it
  with `CronState::new(..)` and its `with_schedule_store`, `with_jitter` and `with_audit`
  methods instead of a struct literal.
