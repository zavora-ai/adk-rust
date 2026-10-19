- **Generation config fields reach the provider or fail by name** (`adk-model`): the OpenAI
  Chat Completions, OpenAI Responses, Anthropic, Bedrock, Groq, DeepSeek, and Azure AI
  Inference clients silently dropped `stop_sequences`, `seed`, `frequency_penalty`,
  `presence_penalty`, and `top_logprobs`. Each now sends the ones its API supports; a request
  that sets any of the others fails before it is sent with `model.unsupported_config`
  (`InvalidInput`) naming them, so remove those fields from the config used with that
  provider. `GeminiModel`, `OllamaModel`, and `OpenRouterClient` have their own entry.
  - Chat Completions (`OpenAIClient`, `OpenAICompatible` and its presets, `AzureOpenAIClient`,
    OpenCode Chat): all five; log probabilities return in `provider_metadata["logprobs"]`.
  - `GroqClient` and `AzureAIClient`: stop sequences, seed, and penalties.
  - `DeepSeekClient`: stop sequences and penalties.
  - `AnthropicClient` and `BedrockClient`: stop sequences.
  - `OpenAIResponsesClient`, and `OpenAIClient` when it routes a GPT-5.6 tool call through
    Responses: `top_logprobs`, returned in `provider_metadata["openai"]["logprobs"]`.
