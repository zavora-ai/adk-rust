- **Gemini Live reports the user's speech onset and end** (`adk-realtime`): the server's
  `voiceActivity` messages (`ACTIVITY_START`, `ACTIVITY_END`) arrive as
  `ServerEvent::SpeechStarted` and `ServerEvent::SpeechStopped` with the offset into the audio
  stream, as OpenAI's `speech_started` and `speech_stopped` do, so `on_speech_started` and
  `on_speech_stopped` handlers and barge-in run on Gemini. They were dropped as `Unknown`.
