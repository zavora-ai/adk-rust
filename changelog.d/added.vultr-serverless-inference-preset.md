- **Vultr Serverless Inference preset** (`adk-model`): `OpenAICompatibleConfig::vultr(key, model)`
  points the OpenAI-compatible client at `https://api.vultrinference.com/v1` with Bearer auth, so
  agents can run on Vultr's hosted open models (for example `qwen3.8-27b`, `glm-5.3`,
  `deepseek-v4.1-flash`) with tool calling and streaming. Rate limits apply per Vultr subscription
  and per model.
