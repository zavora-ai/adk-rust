- **An ADK run needs an existing session** (`adk-server`): `POST /api/run`, and
  `POST /api/run_sse` without a UI protocol, answer `404` with code `session.not_found` for a
  session that does not exist, as adk-python and adk-go do and as the path-style run already
  did. They used to create it. A run with a UI protocol (`a2ui`, `ag_ui`, `mcp_apps`) still
  starts its thread's session. The agent now loads before the session lookup, so an unknown
  app still reports `Cannot load agent` rather than a missing session.

  Migration: create the session first, with
  `POST /api/apps/{app}/users/{user}/sessions/{session}` (or `POST /api/sessions`), then run
  turns in it.
