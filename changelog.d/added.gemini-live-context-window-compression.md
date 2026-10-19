- **Gemini Live context window compression** (`adk-realtime`):
  `RealtimeConfig::with_context_window_compression(trigger_tokens)` makes the Gemini Live setup
  frame send `contextWindowCompression` with a sliding window and, when given, `triggerTokens`.
  Google documents that a Live session without it ends after about 2 minutes once video frames
  are sent and after about 15 minutes with audio only; with it only the ~10-minute connection
  limit remains.
