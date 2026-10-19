- **`GraphAgent` and `NodeTool` key checkpoints by app, user, and session** (`adk-graph`):
  `GraphAgent` runs each session on `session_thread_id(app_name, user_id, session_id)` — the
  three parts joined with `:`, with `%` and `:` percent-encoded inside each — and `NodeTool` runs
  each call on `tool_call_thread_id(app_name, user_id, session_id, function_call_id)`, which
  appends the call id as a fourth part (a call without an id uses the session's thread), instead
  of the bare session id. A user who chooses another user's session id no longer resumes that
  user's run or reads its state. Checkpoints written under a bare session id are not read: a
  session with a paused run, or a paused graph tool call, starts over, and a finished thread's
  state does not carry into the next turn. No fallback read is provided, since the bare key
  cannot tell which user wrote it. Delete those checkpoints, or re-key a session's checkpoints
  with `adk_graph::agent::session_thread_id`.
