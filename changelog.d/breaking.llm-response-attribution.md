- **`LlmResponse` carries `model` and `provider`** (`adk-core`): both are
  `Option<String>` and serialize only when set. Struct literals of
  `LlmResponse` add `model: None, provider: None` or use `..Default::default()`.
- **Bedrock `prompt_token_count` includes cache reads and writes**
  (`adk-model`), matching every other provider; `total_token_count` is
  unchanged.
