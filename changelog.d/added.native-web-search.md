- **`Llm::native_web_search` and DeepSeek web search** (`adk-core`, `adk-model`,
  `adk-tool`, `adk-harness`): a provided `Llm` method reports the provider-executed web
  search a client's transport serves, as `NativeWebSearch` (provider, tool, allowlist
  support). `GeminiModel` reports `google_search` on `generateContent`, `AnthropicClient`
  `web_search` on Anthropic's API and on DeepSeek's Anthropic-compatible endpoint, and
  `OpenAIResponsesClient` `web_search` on OpenAI's API and, for `deepseek-v4-pro`, on
  DeepSeek's Responses endpoint; `MeteredLlm` forwards it and
  `ScriptedLlm::with_native_web_search` sets it in tests. `adk_tool::DeepSeekWebSearchTool`
  declares DeepSeek's `{"type": "web_search"}`. `adk_harness::SearxngSearch` (a SearXNG
  instance's JSON API) and `adk_harness::DeepSeekSearch` (DeepSeek's Search API, keyed
  from `DEEPSEEK_API_KEY`) are `SearchProvider`s for the governed `web_search`.
