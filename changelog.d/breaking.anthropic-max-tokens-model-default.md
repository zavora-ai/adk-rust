- **Model-aware `max_tokens` default** (`adk-model`): `AnthropicConfig::max_tokens` is
  `Option<u32>`; `None` requests 32,000 output tokens from Claude 4 and later models and
  4,096 from older or unrecognized ones (`catalog::anthropic_default_max_tokens`). The
  previous fixed 4,096 left always-thinking Claude 5 models stopping on `max_tokens`
  before the answer. `with_max_tokens(n)` is unchanged; field reads and struct literals
  use `Some(n)`.
