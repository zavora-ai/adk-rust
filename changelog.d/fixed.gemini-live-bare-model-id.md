- **Gemini Live accepts the bare model id** (`adk-realtime`): `GeminiRealtimeModel::new` and
  `GeminiRealtimeSession::connect` on AI Studio send `gemini-3.1-flash-live-preview` as
  `models/gemini-3.1-flash-live-preview`, so the plain id no longer closes the socket with 1007
  ("unexpected model name format"). `model_id()` reports the resource name; names that already
  carry a collection, and Vertex AI names, are sent as given.
