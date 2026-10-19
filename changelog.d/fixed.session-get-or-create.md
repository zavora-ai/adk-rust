- **A failed session lookup no longer recreates the session** (`adk-server`): `/api/run`,
  `/api/run_sse`, and both A2A executors create a session only when `get` reports it not found.
  Any other read failure fails the request (503 for an unavailable backend) instead of
  replacing the stored conversation, and a create that loses a race to a concurrent first
  request reads the session that request created. `POST /api/run/{app}/{user}/{session}` and
  the session endpoints map session errors to their status (404, 409, 400, 503) instead of a
  fixed 404 or 500.
