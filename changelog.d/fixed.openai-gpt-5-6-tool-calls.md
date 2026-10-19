- **Tool calls on GPT-5.6 and later** (`adk-model`): `OpenAIClient` sends requests that declare
  tools for GPT-5.6 and later models through the Responses API, with the same credentials,
  organization, project, base URL, reasoning effort, retry policy, and timeouts, and with
  `store: false`. Chat Completions rejects function tools on these models while reasoning is
  on, so agents with tools on the default `gpt-5.6-terra` failed with HTTP 400. Requests without
  tools, earlier models, reasoning effort `None` (on the client or as the request's `openai`
  `reasoning_effort` extension), and clients built with `OpenAIClient::compatible` stay on Chat
  Completions.
