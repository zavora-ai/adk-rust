- **Streamed Ollama tool-call turns persist once** (`adk-model`): `OllamaModel` streams tool
  calls as partial responses, so a streamed tool-call turn is saved as one event instead of two
  events with the same ID, the second of which a SQLite or Postgres session rejected.
