- **`session_backends` example** (`examples/session_backends`): drives `LlmAgent` and `Runner`
  over the session backends to check Redis Cluster turns with identifiers containing `:`, `{`,
  and `}`, sessions a standalone Redis server already holds, identifier validation on the
  in-memory, encrypted, SQLite, PostgreSQL, Redis, and Redis Cluster backends, and history
  that survives repeated context compaction, against a scripted OpenAI-compatible server and,
  with `OPENAI_API_KEY`, live OpenAI.
