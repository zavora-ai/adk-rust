- **`ScriptedRealtimeModel`** (`adk-realtime` feature `scripted`): a provider-free realtime
  model that answers each client action from a script of `ScriptedStep`s, as
  `ScriptedLlm` stands in for a chat model, with a log of what the client sent. The
  integrated runner also completes a streamed input transcript into
  `InputTranscriptCompleted` on providers that never send one, opens a turn on the first
  delta without `ResponseCreated`, and persists typed text as a user turn.
