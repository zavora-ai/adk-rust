- **`ScriptedLlm` test double** (`adk-model`): answers each `generate_content` call with the
  next `ScriptedResponse` — `text`, `function_call` or `function_calls`, a `stream` of
  responses, `then_error` for a stream that breaks, `error` for a failed call, and
  `with_usage` — and records every request (`requests()`, `last_request()`). `push` appends a
  reply and `remaining()` counts the unused ones. A function call scripted without an ID gets
  `scripted_tc_{call}_{index}`, and a call past the end of the script fails with
  `SCRIPT_EXHAUSTED_CODE` (`model.scripted.exhausted`). `MockLlm`, by contrast, streams its
  whole response list on every call.
