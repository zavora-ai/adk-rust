- **A2A v1 `returnImmediately`, cancellation and waiting tasks follow the spec**
  (`adk-server`):
  - `SendMessage` with `configuration.returnImmediately` returns the `WORKING` task and the
    agent runs on in the background (A2A §3.2.2), where it used to wait for the run to end.
    `RequestHandler::message_send_with` and `CallerScope::message_send_with` take the
    request's `SendMessageConfiguration`, and both bindings pass it through.
  - `CancelTask` stops the agent run, model call included, and the run records no partial
    output. It used to change only the stored state. A cancelled stream ends on a
    `CANCELED` status. The runner configuration's own cancellation token still stops a run.
  - A task that stops for a long-running or held call names it in its `INPUT_REQUIRED`
    status: each call, with its interim result, as a data part tagged
    `adk_type: function_call` / `function_response`, the shape adk-python and adk-go use.
    The caller answers with a tagged `function_response` part and the task's `taskId`.
    Tagged data parts are read as function calls and responses, and
    `v1::convert::tagged_function_call_part` and `tagged_function_response_part` build
    them. `V1Executor::transition_state_with_parts` sets a status message with any parts.
