- **`LlmResponse` carries `model` and `provider`** (`adk-core`): both are `Option<String>`,
  serialize only when set, and are filled by every `adk-model` provider. Struct literals of
  `LlmResponse` add `model: None, provider: None` or end with `..Default::default()`.
- **Bedrock `prompt_token_count` includes cache reads and writes** (`adk-model`): streamed
  and unary, as `AnthropicClient` already reported it, so `PricingCatalog` bills each cached
  token once; `total_token_count` is unchanged.
