- **`ResponsesTransport`, `OpenAIResponsesConfig::transport` and `with_transport` are removed**
  (`adk-model`): the option was never read, so selecting `ResponsesTransport::WebSocket` kept
  HTTP without notice. `OpenAIResponsesClient` always uses HTTP; WebSocket mode is
  `WsTransport` (`openai-ws` feature). Struct literals of `OpenAIResponsesConfig` drop the
  `transport` field; a serialized config that still carries `transport` deserializes with the
  field ignored.
- **`adk_gemini::ClientError` gains `ErrorInResponse`** (`adk-gemini`):
  `ErrorInResponse { code: Option<u16>, status: Option<String>, message: Option<String>,
  description: String }` reports an `error` object inside a successful response or stream
  event; `description` holds the body or event data. Exhaustive `match` statements on
  `ClientError` need an arm for it.
