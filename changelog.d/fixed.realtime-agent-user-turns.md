- **`RealtimeAgent` saves the user's spoken turns** (`adk-realtime`): under `Runner`, each input
  transcript becomes a user event (author `user`, as the runner saves the run's text message)
  ahead of the model's reply to it, so session history and memory see both sides of a voice
  conversation. OpenAI's `InputTranscriptCompleted` is saved once; Gemini Live's streamed
  transcript, which is never completed, is saved where the model's reply starts or ends, and a
  turn still unsaved when the session ends is saved then.
