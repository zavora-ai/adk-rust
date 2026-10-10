- **Model and tool payloads stay out of telemetry by default** (`adk-agent`): with
  `RunConfig::record_payloads` off, the `call_llm` span records
  `[omitted: set RunConfig::record_payloads to record payloads]` for the request and
  response, and the DEBUG `tool_call`/`tool_result` logs record the same marker for tool
  arguments and results. They previously recorded the first `trace_payload_max_bytes`
  (2 KB) of each payload, which holds a short prompt or tool result in full.
- **Every span of a run carries its owner** (`adk-telemetry`): `AdkSpanLayer` copies
  `adk.app_name` and `adk.user_id` from `agent.execute` to its child spans, so the
  server's debug routes refuse another user's `call_llm` and tool spans, not only their
  `agent.execute` span.
