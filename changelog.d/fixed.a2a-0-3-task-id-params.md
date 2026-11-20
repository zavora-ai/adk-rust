- **A2A 0.3 `tasks/get` and `tasks/cancel` take the specification's `id`** (`adk-server`):
  `{"id": "…"}` was refused with `-32602 missing field taskId`. `TasksGetParams` and
  `TasksCancelParams` read `id`, and still accept `taskId`; they serialize as `id`.
