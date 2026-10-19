- **Realtime after-tool failures withhold the result** (`adk-realtime`): when an
  `IntegratedRealtimeRunner` `after_tool_call` plugin failed, the tool's unscreened result went
  to the model, so a redaction plugin whose backend was down leaked what it should have
  removed; a failed `RealtimeAgent` after-tool callback replaced the result with its error
  text. Both now answer with `adk_core::result_withheld_response`, which states that the tool
  ran and its result is withheld, so the model neither sees the result nor repeats the call.
