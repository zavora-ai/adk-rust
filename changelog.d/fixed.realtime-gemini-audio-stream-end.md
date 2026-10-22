- **Gemini Live closes a turn when the client stops sending audio** (`adk-realtime`):
  under automatic activity detection, `commit_audio` now sends `audioStreamEnd` after
  flushing the buffer, so the server releases the audio it cached and ends the user's
  turn. Before, a recorded or paused stream left the turn open and no transcript or
  response followed. In manual mode `commit_audio` still only flushes.
