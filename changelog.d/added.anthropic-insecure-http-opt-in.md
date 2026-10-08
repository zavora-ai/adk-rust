- **Plain-HTTP base URLs for internal gateways** (`adk-anthropic`, `adk-model`):
  `Anthropic::allow_insecure_http()` and `AnthropicConfig::allow_insecure_http()` accept
  an `http://` base URL on a non-loopback host and log a warning naming the host. On
  `Anthropic`, call it before `with_base_url`, which validates when called. A base URL
  from `ANTHROPIC_BASE_URL` is acknowledged by `ANTHROPIC_ALLOW_INSECURE_HTTP=1` instead.
  Without the opt-in, the validation error now names it (#700).
