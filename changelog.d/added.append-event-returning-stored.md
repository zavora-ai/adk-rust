- **The run reads what the service stored** (`adk-session`, `adk-runner`):
  `SessionService::append_event_returning_stored` appends an event and returns it as stored; the
  default returns the event it was given. The runner persists the user's message and agent events
  through it and replaces only that event in its view, so a workflow's next sub-agent, a transfer
  target and the `LlmAgent`'s copy of the user's message see what a redacting service stored. The
  history window, compaction and screened content are kept, and other writers' events stay out of
  the run. `ConsentGatedSessionService` returns the event without the profile keys it filtered.
