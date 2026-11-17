- **Fast mode on Claude Fable 5.2 and Mythos 5.2, and platform model IDs** (`adk-model`):
  `AnthropicConfig::with_fast_mode(true)` is accepted for `claude-fable-5-2` and `claude-mythos-5-2`
  instead of failing with `model.anthropic.fast_mode_unsupported`. A platform-prefixed ID such as
  `anthropic.claude-sonnet-5-5` or `us.anthropic.claude-opus-4-7-v1` follows the sampling and
  fast-mode rules of the model it names, instead of sending `temperature`, `top_p`, or `top_k` the
  API rejects.
