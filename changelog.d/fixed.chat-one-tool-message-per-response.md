- **One `tool` message per function response on Chat Completions** (`adk-model`): a content
  holding several function responses, such as a parallel batch refused by a run budget or a
  history written by another runtime, was sent as one `tool` message answering only the first
  call, so OpenAI rejected that turn and every later one with HTTP 400. The OpenAI-compatible
  client and its presets, OpenCode Chat, Azure OpenAI, Azure AI Inference, Groq, DeepSeek, and
  OpenRouter now send one `tool` message per response, and text beside the responses as a
  following user message; function responses in a user-role content are sent as `tool`
  messages too.
