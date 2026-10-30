- **Session view refresh** (`adk-runner`): `Runner::with_session_event_refresh(true)` reads the
  session back from the session service after the user's message and after every non-partial
  event the runner persists, and uses the stored events as the run's view, so later agents in
  the run see what the service stored. Every fragment still streams to the caller, partial
  chunks are not added to the view, and a failed read ends the run instead of continuing on a
  stale view. Off by default.
