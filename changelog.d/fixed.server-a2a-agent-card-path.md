- **A2A agent card at `/.well-known/agent-card.json`** (`adk-server`): `create_app_with_a2a`
  and `ServerBuilder::with_a2a` serve the agent card at the well-known path A2A 0.3.0 and
  later define, as well as at `/.well-known/agent.json`. The bundled v1 client
  (`A2aV1Client::resolve_agent_card`) fetches the newer path and got a 404 from these servers.
