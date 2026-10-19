- **Responses function tools keep optional parameters** (`adk-model`): `OpenAIResponsesClient`
  sends converted function declarations with `strict: false`; it left the field out, so the API
  applied its strict-mode defaults to schemas written with optional parameters. A native
  `x-adk-openai-tool` declaration of a type the client does not model, sent as a function tool,
  keeps its own boolean `strict`, or `false` when it sets none.
