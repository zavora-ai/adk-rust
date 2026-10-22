- **Gemini Live interruptions reach barge-in handlers** (`adk-realtime`):
  `serverContent.interrupted`, the server's report that the user's speech cut the model off,
  was dropped. It now arrives as `ServerEvent::SpeechStarted`, the event barge-in handlers
  flush playback on, unless a `voiceActivity` `ACTIVITY_START` already reported that speech.
