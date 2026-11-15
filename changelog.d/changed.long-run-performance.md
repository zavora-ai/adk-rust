- **Long sessions no longer copy their whole history on every turn** (`adk-session`,
  `adk-runner`): `InMemorySessionService::get` shares the stored events with the session it
  returns and copies only the `num_recent_events` window, outside the lock every session
  shares, so a `RunConfig::history_max_events` window bounds a turn's cost and memory. The
  runner moves the loaded events into the invocation with the new provided method
  `Session::take_events`, which every `adk-session` backend implements, instead of keeping the
  loaded session and a copy of it; `EncryptedSession` decrypts events in place.
- **SQLite sessions use write-ahead logging and an index by session** (`adk-session`):
  `SqliteSessionService::new` switches a file database to `journal_mode = WAL` and keeps
  `synchronous = FULL`, so every commit is still synced to disk. Schema version 3 adds
  `idx_events_session_ts`, so loading, rewinding or deleting a session reads only its rows
  however many sessions share the file.
- **Fewer copies of the request per model call** (`adk-agent`, `adk-plugin`, `adk-core`):
  before-model callbacks and `PluginManager::run_before_model` take the request instead of a
  copy, and the agent keeps a copy for `on_model_error` only while a hook can answer a failure
  (the new provided method `InvocationHooks::handles_model_errors`; `PluginManager` answers
  `true` only with an `on_model_error` plugin). `held_tool_calls` returns at once when no event
  holds a call.
