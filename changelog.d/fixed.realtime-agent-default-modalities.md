- **A default `RealtimeAgent` runs on Gemini Live** (`adk-realtime`): the builder's default
  output modalities were `["text", "audio"]`, which Gemini Live refuses (it accepts one
  response modality, and `gemini-3.1-flash-live-preview` only audio), so the session closed
  during setup and nothing was said. Unset modalities now leave the choice to the provider — audio,
  with a transcript, on Gemini Live and on the OpenAI Realtime API. `modalities(vec)` still
  sets them explicitly.
