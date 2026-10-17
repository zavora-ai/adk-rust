- **`ScriptedLlm` test double** (`adk-model`): answers each `generate_content` call with the
  next `ScriptedResponse` — text, function calls, a stream of responses, a stream that ends in
  an error, a failed call, or reported usage — and records every request
  (`requests()`, `last_request()`). Function calls scripted without an ID get
  `scripted_tc_{call}_{index}`, and a call past the script fails with
  `SCRIPT_EXHAUSTED_CODE`. `MockLlm`'s docs now state that it replays every response on every
  call.
