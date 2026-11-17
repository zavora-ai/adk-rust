- **Graph interrupts carry an id and an answer schema** (`adk-graph`): `Interrupt::Dynamic`
  gains `id` and `response_schema`, `StreamEvent::Interrupted` gains `interrupt_id`, and
  `StreamEvent::NodeInterrupt` gains `response_schema`; a
  pattern naming every field ends with `..`, and a literal sets them to `None` (or uses
  `interrupt`, `interrupt_with_data` and `StreamEvent::interrupted`). `GraphError` is
  `#[non_exhaustive]` and gains `ResumeRejected`, so a `match` on it needs a wildcard arm.
