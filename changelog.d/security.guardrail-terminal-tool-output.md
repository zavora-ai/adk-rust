- **Output guardrails screen a tool result that ends the turn** (`adk-agent`): a tool that
  set `skip_summarization` or `escalate` made its result the final response, but output
  guardrails ran only on model text, so the result reached the client and the session
  unscreened. `LlmAgent` now runs output guardrails on each function response of such a
  result, as its JSON text, before the event is emitted: a block ends the run with an error,
  and a transformation such as redaction replaces the response.
