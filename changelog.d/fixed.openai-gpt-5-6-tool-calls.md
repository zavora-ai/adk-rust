- **Tool calls on GPT-5.6 and later** (`adk-model`): `OpenAIClient` sends requests that
  declare tools for GPT-5.6 and later models through the Responses API, with the same
  credentials, base URL, reasoning effort, and retries and `store: false`. Chat
  Completions rejects function tools on these models while reasoning is on, so agents with
  tools on the default `gpt-5.6-terra` failed with HTTP 400. Requests without tools,
  earlier models, and reasoning effort `None` stay on Chat Completions.
