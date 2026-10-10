- **A timed-out non-idempotent call reports an unknown outcome** (`adk-agent`): the
  function response is `{"status": "outcome_unknown", "detail": …}` instead of
  `{"error": …}`, so the model does not read it as an invitation to call again.
- **`RunConfig::action_ledger`** (`adk-core`): a `RunConfig` struct literal without
  `..Default::default()` must add the field.
- **Fallback function call IDs are unique per model turn** (`adk-agent`): a call the
  provider sent without an ID was `{invocation}_{name}_{index}`, which two model turns of
  one invocation shared. It is now `{invocation}_{turn}_{name}_{index}`.
- **A cancelled run persists answers to its in-flight tool calls** (`adk-runner`): a
  call whose run was interrupted mid-flight was dropped from the next model request, so
  the model never learned that a payment or other side effect may have happened. The
  runner now persists one `{"status": "outcome_unknown"}` function response for each
  call still in flight, keeping one response per call, and yields it before the stream
  ends.
