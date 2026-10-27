- **Graph run records** (`adk-graph`; spec 3.3 §12): a step that fails is always
  checkpointed with its record, so a workflow observer no longer shows a failed run as
  working; the step after an `interrupt_after` gate takes the next step label instead of
  sharing the gated step's; and the `fork.paused` and `fork.unknown_key` messages no
  longer carry a run of spaces.
