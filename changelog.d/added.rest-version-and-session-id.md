- **REST: `GET /api/version`, and a session id in the create body** (`adk-server`):
  - The REST API serves `GET /api/version`, which returns `{version, language}`, as
    adk-python and adk-go do.
  - `POST /apps/{app}/users/{user}/sessions` uses a `sessionId` sent in the body when the
    path names none. It used to ignore it and generate an id.
    `CreateSessionBodyRequest` gains `session_id`.
