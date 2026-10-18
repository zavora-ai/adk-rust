- **Images a tool returns reach every provider** (`adk-model`): Anthropic and Bedrock put them
  inside the tool result (Bedrock also carries supported documents), and Groq, DeepSeek, Azure
  AI Inference, and Ollama, whose tool messages carry only text, send them in a user message
  after the tool messages, as the OpenAI clients already did. They were dropped before.
