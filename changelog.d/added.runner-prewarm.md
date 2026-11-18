- **Prewarm** (`adk-core`, `adk-anthropic`, `adk-model`, `adk-agent`, `adk-harness`,
  `adk-runner`, `adk-cli`): `Runner::prewarm` readies an agent before its first message —
  `Llm::prewarm` opens the model's pooled connections (`Anthropic::prewarm`, a free
  `GET /v1/models` on each client), `Agent::prewarm` reaches every agent in the tree, a harness
  starts its MCP servers, and `Prewarm::session` reads the session and refuses an archived one.
  It is refused while frozen and stopped by a time budget. The `adk-rust` REPL prewarms while the
  person types, and `adk-rust up` prewarms the primary agent at boot.
