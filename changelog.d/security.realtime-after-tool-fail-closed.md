- **Realtime after-tool plugin failures withhold the result** (`adk-realtime`): when the
  `after_tool_call` pipeline of an `IntegratedRealtimeRunner` returned an error, the tool's
  unscreened result went to the model, so a redaction plugin whose backend was down leaked
  what it should have removed. The model now receives an error that states the tool ran and
  that its result is withheld, so it neither sees the result nor repeats the call.
