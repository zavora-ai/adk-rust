- **A2A agent card at `/.well-known/agent-card.json`** (`adk-server`): `create_app_with_a2a`
  and `ServerBuilder::with_a2a` serve the agent card at the well-known path A2A 0.3.0 and
  later define, as well as at `/.well-known/agent.json`, so these servers are discoverable
  by A2A 0.3+ clients. The served card advertises no `supportedInterfaces`, so the bundled
  v1 client (`A2aV1Client`) resolves it but has no endpoint to send requests to.
