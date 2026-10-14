- **Anthropic request adapter** (`adk-model`): `AnthropicClient::with_request_adapter`
  adapts the complete Messages JSON body and outgoing headers for each attempt,
  streaming included. Without an adapter, the client sends the same requests. A body
  that changes `stream`, fails validation, or adds a top-level field
  `MessageCreateParams` does not model fails with `model.anthropic.request_adapter`
  before anything is sent.
