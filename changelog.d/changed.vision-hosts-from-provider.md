- **Remote vision backends take their hosts from the model** (`adk-core`, `adk-model`,
  `adk-gemini`, `adk-vision`): `Llm::data_destinations` names each provider a model sends to,
  by the id its responses carry in `LlmResponse::provider`, with the hosts of its configured
  endpoint (`DataDestination`); every `adk-model` provider implements it — Gemini Studio and
  Vertex AI by location, OpenAI and the OpenAI-compatible presets, Azure, Anthropic, DeepSeek,
  Groq, Ollama, Bedrock by region, OpenRouter, Muse and OpenCode — and wrappers forward it.
  `LlmDescriber::remote(llm)` takes its hosts and providers from the model;
  `LlmDescriber::remote_with_hosts` names hosts by hand. An `[[egress.rule]]` may name a
  `provider` instead of a `host`, matching every host of that provider
  (`EgressConfig::with_provider_rule`, `RuleTarget`, `BackendDescriptor::providers`).
  `EgressGate::decide` takes the backend's descriptor, and `EgressConfig::decide` its
  providers.
