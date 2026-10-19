- **`openai-realtime` and `gemini-live` umbrella features** (`adk-rust`): `realtime` links
  `adk-realtime` without a transport, and the only transports the umbrella forwarded were
  `vertex-live`, `livekit` and `openai-webrtc` (which needs cmake). `openai-realtime` forwards
  `adk-realtime/openai` and `gemini-live` forwards `adk-realtime/gemini`, the WebSocket
  transports. The tiers are unchanged.
