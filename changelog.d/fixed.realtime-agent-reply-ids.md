- **Each `RealtimeAgent` reply on Gemini Live is saved under its own id** (`adk-realtime`):
  Gemini Live streams replies without an item id, so every reply of a run was saved as
  `{invocation}:realtime-transcript:` (or `:realtime-text:`), and a session backend that keys
  events by id, such as SQLite, refused the second reply and ended the run with
  `UNIQUE constraint failed`. A reply without an item id is now saved under
  `reply-{uuid}`, minted when the reply starts.
