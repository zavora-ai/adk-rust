- **Structured output on Anthropic, Groq and Azure AI Inference** (`adk-model`):
  `GenerateContentConfig::response_schema` reaches `AnthropicClient` as `output_config.format`
  (`json_schema`, beside any effort), with every object closed and the bounds structured outputs
  reject dropped, and `GroqClient` and `AzureAIClient` as a `json_schema` `response_format`.
  `LlmAgentBuilder::output_schema`, the `adk-eval` judges and `adk_core::decide` now constrain
  replies on these providers instead of relying on the prompt alone.
