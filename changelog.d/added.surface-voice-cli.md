- **Voice in `adk-rust serve`, `up` and the terminal** (`adk-cli`, feature `voice`; spec 3.3
  §5.6): a harness marked `--pack voice` gets the home's voice view on every realtime model
  whose key resolves — Gemini Live with context window compression, the OpenAI Realtime API —
  the resolved model's provider first. `HarnessLauncher::with_voice_model` offers a model to a
  launcher built in code. With `tui` and `voice-desktop`, `adk-rust --voice` on a terminal
  opens the terminal with a voice conversation beside the thread, through the default
  microphone and speaker; a first turn, `--voice-input` or `--voice-output` keep the console
  voice. The inline terminal prints what voice heard, each countdown and a call voice cannot
  approve into the scrollback (`adk-surface`).
