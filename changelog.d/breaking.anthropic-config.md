- **Model-aware `max_tokens` default** (`adk-model`): `AnthropicConfig::max_tokens` is
  `Option<u32>`. `None`, the default, requests `ANTHROPIC_DEFAULT_MAX_TOKENS` (32,000) output
  tokens from Claude 4 and later model IDs (`claude-opus-*`, `claude-sonnet-*`,
  `claude-haiku-*`, `claude-fable-*`, `claude-mythos-*`) and `ANTHROPIC_LEGACY_MAX_TOKENS`
  (4,096) from others (`catalog::anthropic_default_max_tokens`), instead of a fixed 4,096 that
  left always-thinking Claude 5 models stopping on `max_tokens` before the answer.
  `with_max_tokens(n)` is unchanged; field reads and struct literals use `Some(n)`.
- **`AnthropicConfig` gains public fields** (`adk-model`): `request_timeout_secs`,
  `prompt_cache_ttl`, and `thinking_display`, set by `with_request_timeout`,
  `with_prompt_cache_ttl`, and `with_thinking_display`. A struct literal must add them or end
  with `..Default::default()`.
