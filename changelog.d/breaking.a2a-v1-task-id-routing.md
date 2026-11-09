- **An A2A v1 message's `taskId` must name a task it can continue** (`adk-server`): a
  message with a `taskId` used to start a new task when the server ignored it. Now
  `SendMessage` and `SendStreamingMessage` continue that task, which must be one of the
  caller's and must be waiting on the caller (`INPUT_REQUIRED` or `AUTH_REQUIRED`), and
  infer the `contextId` from it (A2A §3.4). An unknown task gets `TaskNotFoundError`, a
  `contextId` that is not the task's gets `InvalidParams`, and a task that has ended or is
  still running gets `UnsupportedOperationError`. A message without a `taskId` still
  continues the caller's `INPUT_REQUIRED` task in its `contextId`.

  Migration: send a follow-up without the `taskId` to start a new task in the same context.
