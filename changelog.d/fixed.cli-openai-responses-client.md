- **`adk-rust --provider openai` runs GPT-5.6 and later on the Responses API** (`adk-cli`,
  spec 3.1 §7.20): the CLI built the Chat Completions client for every OpenAI model, so an
  OpenAI session had no provider web search and no Responses-only features. It now builds
  `OpenAIResponsesClient` (responses not stored, as on Chat Completions) for GPT-5.6 and
  every later GPT generation, the models whose tool calls `OpenAIClient` already sends to the
  Responses API, and keeps `OpenAIClient` for older models.
