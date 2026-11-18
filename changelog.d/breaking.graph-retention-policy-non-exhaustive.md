- **`RetentionPolicy` is `#[non_exhaustive]`** (`adk-graph`): it gains `pinned`, so a struct
  literal no longer compiles. Build it with `RetentionPolicy::keep_last`, `max_age` and their
  `with_*` methods.
