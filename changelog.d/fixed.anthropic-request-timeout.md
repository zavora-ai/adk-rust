- **Ten-minute Anthropic request timeout** (`adk-model`, `adk-anthropic`): non-streaming
  requests, such as an agent called through `AgentTool`, are bounded at 10 minutes instead
  of 60 seconds, matching the official SDKs. `AnthropicConfig::with_request_timeout` sets
  the bound, `Anthropic::timeout` reads it, and connecting stays capped at 60 seconds.
