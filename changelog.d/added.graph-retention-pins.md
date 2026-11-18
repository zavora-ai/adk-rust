- **Retention pins and the resume set** (`adk-graph`): `RetentionPolicy::pinning` keeps
  named checkpoints whatever the limits say, and `checkpoint::needed_to_resume` reports the
  checkpoints no policy removes — the newest, open pauses, effect-log entries. The
  `prune_keeps_what_a_resume_needs` conformance case runs on every backend, and
  `SealedCheckpointer` now runs the whole conformance suite.
