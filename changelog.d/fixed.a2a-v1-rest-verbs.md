- **A2A v1 REST cancel and subscribe use the spec's paths** (`adk-server`): the HTTP+JSON
  binding served `POST /tasks/{id}/cancel` and `POST /tasks/{id}/subscribe`, so a spec client
  (including adk-server's own `cancel_task_rest`) got 404. It now serves
  `POST /tasks/{id}:cancel` and `GET /tasks/{id}:subscribe` (A2A §11.3), and also accepts
  `POST /tasks/{id}:subscribe`. The old paths still work.
