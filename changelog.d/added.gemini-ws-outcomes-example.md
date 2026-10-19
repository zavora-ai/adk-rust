- **`gemini_ws_outcomes` example** (`examples/gemini_ws_outcomes`): drives `GeminiModel`
  through `LlmAgent` and `Runner`, and `WsTransport` directly, against scripted local
  upstreams that send error objects, cut streams, blocked prompts, block-type finish reasons,
  incomplete and failed responses, early closes, and rejected handshakes, and checks that each
  ends with its real outcome; live Gemini and OpenAI WebSocket runs cover the happy paths when
  keys are set.
