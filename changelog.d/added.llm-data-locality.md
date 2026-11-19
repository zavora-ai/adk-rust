- **`Llm::data_locality`** (`adk-core`, `adk-model`, `adk-mistralrs`): a model says where a
  call's request data goes — `DataLocality::InProcess`, `Loopback` or `Remote` (the default).
  mistral.rs models are `InProcess`; `OpenAICompatible`, `OpenAIClient`,
  `OpenAIResponsesClient` and `OllamaModel` at `localhost` or a loopback address are
  `Loopback`, except Ollama cloud models, which the local server forwards. `MeteredLlm`,
  `OpenCodeClient` and the harness, ACP and benchmark wrappers forward it, and `FallbackLlm`
  reports the least local model of its chain. `DataLocality::of_url` classifies a base URL, and
  `ScriptedLlm::with_data_locality` stands in for a local model in tests.
