- **Streamed turns persist once and whole** (`adk-agent`): an `after_model_callback` that
  returns a new `LlmResponse` keeps the replaced chunk's `partial` and `turn_complete` flags and,
  unless it sets them, its `finish_reason` and `usage_metadata`, so a rebuilt streamed delta no
  longer persists several copies with one event id (a primary-key failure on SQLite and
  Postgres) or drops usage. A stream that ends on a partial chunk gets one closing event with
  the whole response, so the session and the next turn keep the answer.
