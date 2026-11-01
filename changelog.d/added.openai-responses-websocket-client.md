- **`OpenAIResponsesClient` can stream over WebSocket mode** (`adk-model`, `openai-ws`):
  `with_websocket` streams generations over a pooled `WsTransport`, and opt-in
  `with_websocket_fallback(true)` sends a generation over HTTP only when the WebSocket failed
  before writing it. `WsTransport::new` creates a transport that connects on first use.
