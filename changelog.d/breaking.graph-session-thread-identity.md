- **`GraphAgent` and `NodeTool` key checkpoints by app, user, and session** (`adk-graph`):
  the checkpoint thread id is `session_thread_id(app_name, user_id, session_id)` — the
  three parts joined with `:`, with `%` and `:` percent-encoded inside each — instead of
  the bare session id, so a user who chooses another user's session id no longer
  resumes that user's run or reads its state. Checkpoints written under a bare session
  id are not read: a session with a paused run starts over on its next turn, and a
  finished thread's state does not carry into the next turn. No fallback read is
  provided, since the bare key cannot tell which user wrote it. Delete those
  checkpoints, or re-key them with `adk_graph::agent::session_thread_id`.
