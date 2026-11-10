- **A2A v1 `SubscribeToTask` follows a running task** (`adk-server`): subscribing used to
  return a snapshot of the task and close. The stream now starts with the task, then
  carries its status changes and artifacts as they happen, until the task ends or stops for
  its caller. This works whichever request runs the task, for example a `returnImmediately`
  send, another client's stream, or a resumed task. With no run in progress it is still the
  snapshot. A finished task gets `UnsupportedOperationError` (-32004), as A2A §3.1.6
  requires, where it used to get `TaskNotCancelableError` (-32002).

  New on `V1Executor`: `subscribe` and `publish` for a task's live updates, and
  `persist_artifact`, which records an artifact without announcing it.
