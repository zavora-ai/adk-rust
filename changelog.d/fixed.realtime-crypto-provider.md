- **Realtime connects work without installing a crypto provider** (`adk-realtime`): with the
  `gemini` feature both rustls providers are compiled in, so the first Gemini Live connect
  panicked with "Could not automatically determine the process-level CryptoProvider" unless
  the application installed one. The Gemini Live, OpenAI Realtime and LiveKit connects now
  install the workspace default (`aws-lc-rs`) through `adk_core::ensure_crypto_provider()`; a
  provider the application installed first is kept.
