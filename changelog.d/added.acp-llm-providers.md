- **ACP configurable LLM providers** (`adk-acp`): adk-acp enables the SDK's
  `unstable_llm_providers` surface. `AcpServerConfigBuilder::providers` takes an
  `AcpProviders` hook. With one configured, the server advertises `providers` and answers
  `providers/list`, `providers/set` and `providers/disable`, checking each request
  against the hook's listing: a known provider, a supported protocol, a base URL, and no
  disabling of a required provider. `ModelProviders` is a ready-made hook for ADK-Rust
  agents. Each `ModelProvider` pairs a `SwitchableModel`, an `Llm` whose backing model
  can be replaced while the agent runs, with a factory that builds the new model from a
  client's `ProviderSettings`. Headers are never listed or logged. On the client,
  `AcpConnection` adds `list_providers`, `set_provider` and `disable_provider`, each
  checked against the agent's `providers` capability.
