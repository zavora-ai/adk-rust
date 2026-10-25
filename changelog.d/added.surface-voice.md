- **Voice and video on every surface** (`adk-surface`, feature `voice`; spec 3.3 §5.6): a
  spoken "allow once" or "deny" is the engine's own answer, given through
  `SurfaceEngine::decide_spoken` and bound to the decision prompted on screen when the person
  started speaking. "allow once" waits out the cancel window; high-risk and money calls take
  only "deny" and point to the screen. Every `surface.decide` record of a spoken answer
  carries the transcript's SHA-256 (`transcriptDigest`) and never the words or the audio.
  `VoiceHub` opens one realtime session of a `Pack::Voice` harness per conversation, in its
  own session; `VoiceSource` resumes a call a live conversation holds there, never on the
  text runner. With `web`, `voice::router` and `voice::socket_routes` serve the voice view at
  `/ui/voice` — microphone, camera and screen switches, single-use socket tickets — and the
  page's content security policy names the socket's host; with `tui`,
  `TerminalSurface::voice` adds a voice line to the full-screen terminal. Examples
  `scripted_voice_home` and `voice_home`.
- **Context window compression for the harness's voice sessions** (`adk-harness`):
  `VoiceConfig::with_context_window_compression` keeps a Gemini Live session that receives
  camera or screen frames from ending after about two minutes.
