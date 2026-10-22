- **`RealtimeAgentBuilder::transcription()`** (`adk-realtime`): asks the provider to transcribe
  the user's speech (and, on Gemini Live, the model's), as `RealtimeConfig::with_transcription`
  does, so a `RealtimeAgent` receives the transcripts it saves as the user's turns. Without it
  the agent's session config is unchanged.
