- **A2A tasks stop in `input-required` and `auth-required`** (`adk-server`): a run that ended on
  a tool call held for confirmation, or on a long-running call, was reported `completed`, so a
  peer never knew the task waited on it. Both the pre-v1 executor and the v1 request handler,
  buffered and streaming, now end such a task in `input-required`, and a run that issues an
  `adk_request_credential` call in `auth-required`. The pre-v1 `TaskState` gains `AuthRequired`.
