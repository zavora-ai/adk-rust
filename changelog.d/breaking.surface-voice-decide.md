- **A spoken answer needs its transcript** (`adk-surface`): `SurfaceEngine::execute` refuses a
  `Command::Decide` whose caller is on the voice row (surface or origin voice) with
  `surface.invalid`; use `SurfaceEngine::decide_spoken`, which records the transcript's
  digest. `adk_harness::VoiceConfig` gains the public field `context_window_compression`, so a
  struct literal of it needs the field (or `VoiceConfig::new`).
