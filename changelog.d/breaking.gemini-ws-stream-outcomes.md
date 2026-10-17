- **`ResponsesTransport` and `OpenAIResponsesConfig::with_transport` are removed**
  (`adk-model`): the option was never read, so selecting `ResponsesTransport::WebSocket`
  kept HTTP without notice. `OpenAIResponsesClient` always uses HTTP; use `WsTransport`
  (`openai-ws` feature) for WebSocket mode. A serialized config that still carries
  `transport` deserializes with the field ignored.
- **`adk_gemini::ClientError` gains `ErrorInResponse`** (`adk-gemini`): an `error` object
  inside a successful response is reported with its `code`, `status`, and `message`.
  Exhaustive `match` statements on `ClientError` need an arm for it.
