- **`A2aClient` fetches `/.well-known/agent-card.json` first** (`adk-server`):
  `A2aClient::resolve_agent_card`, and with it `RemoteA2aAgent`, requests the A2A 0.3+ path
  and falls back to `/.well-known/agent.json` when that path returns 404.
