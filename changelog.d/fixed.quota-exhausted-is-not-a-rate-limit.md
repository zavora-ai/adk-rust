- **An account out of quota or credit is no longer reported as a rate limit** (`adk-model`,
  `adk-server`): OpenAI answers HTTP 429 both for a rate limit and for an account with no
  credit. A response whose error `code` or `type` is `insufficient_quota`,
  `credit_balance_exhausted`, `billing_hard_limit_reached`, or an organization or project spend
  limit, Anthropic's `billing_error`, and any HTTP 402 Payment Required are now
  `ErrorCategory::ResourceExhausted`, not retried, with the remedy in the message ("add credit
  or raise the limit with the provider"). This covers the OpenAI-compatible, Azure OpenAI,
  Responses (HTTP, stream and WebSocket), DeepSeek, Azure AI, Anthropic and OpenRouter clients;
  the compatible, Azure OpenAI and Responses clients report it under their own
  `*.quota_exhausted` code. Previously the compatible client retried it three times as
  `RateLimited` and the Responses stream reported it as `InvalidInput`. The server's generic
  message for `resource_exhausted` reads "A budget, quota or credit balance is used up".
