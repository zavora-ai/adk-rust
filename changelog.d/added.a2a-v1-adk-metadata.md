- **A2A v1 tasks carry ADK metadata** (`adk-server`): a task the server runs names its ADK
  app, user and session in `adk_app_name`, `adk_user_id` and `adk_session_id`, and is marked
  with the ADK extension (`https://google.github.io/adk-docs/a2a/a2a-extension/`: `true`), as
  adk-python and adk-go mark theirs. Every status and artifact update carries the same
  metadata. The text of a `FAILED` status is marked `adk_is_error_message`.
  `A2aV1Server`'s card declares the ADK extension.

  New: `V1Executor::create_task_with_metadata`, `v1::convert::error_message_part`, and the
  `ADK_*_METADATA_KEY` and `ADK_EXTENSION_URI` constants.
