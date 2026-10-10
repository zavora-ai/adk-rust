- **Prompt cache TTL for the Anthropic provider** (`adk-model`):
  `AnthropicConfig::with_prompt_cache_ttl(CacheTtl::one_hour())` sets the lifetime of
  both automatic cache breakpoints, so conversations that wait more than five minutes
  between requests keep their cached prefix. `CacheTtl` is re-exported from
  `adk_model::anthropic`.
