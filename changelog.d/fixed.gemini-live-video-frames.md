- **Gemini Live camera frames reach the model** (`adk-realtime`): `send_video_frame` sends
  `realtimeInput.video` instead of the deprecated `realtimeInput.mediaChunks`, which
  `gemini-3.8-live` drops without an error, so the model answered as if it saw nothing.
