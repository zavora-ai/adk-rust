- **A non-idempotent call that times out or panics reports an unknown outcome**
  (`adk-agent`): `LlmAgent` answers it with `{"status": "outcome_unknown", "detail": …}`
  instead of `{"error": …}`, and `CodeActAgent` raises the same payload into the script, so
  the model does not read the failure as an invitation to call again. A tool that declares
  no `Tool::effect()` and is not `is_read_only()` is `NonIdempotent`.
- **A cancelled run answers its in-flight tool calls** (`adk-runner`): a call whose run was
  interrupted mid-flight was dropped from the next model request, so the model never learned
  that a payment or other side effect may have happened. The runner now persists one
  `{"status": "outcome_unknown"}` function response for each call still in flight, keeping
  one response per call, and yields it before the stream ends.
