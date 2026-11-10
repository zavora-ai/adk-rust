- **`with_a2a` serves A2A v1** (`adk-server`, feature `a2a-v1`): `ServerBuilder::with_a2a`,
  `create_app_with_a2a` and `A2aServer` used to serve only A2A 0.3, so a v1 endpoint had to
  be assembled by hand. They now also serve the root agent over A2A v1:
  - the JSON-RPC binding at `/a2a/v1`;
  - the HTTP+JSON binding under `/a2a/v1/rest`;
  - the v1 agent card at `/.well-known/agent-card.json`.

  A2A 0.3 keeps its `/a2a` and `/a2a/stream` endpoints, and its card moves to
  `/.well-known/agent.json` only. The v1 card lists the 0.3 endpoint as an interface with
  `protocolVersion` `0.3`. The v1 card's skills come from the agent. Without `a2a-v1`,
  nothing changes.

  `A2aClient::resolve_agent_card` falls back to `agent.json` when `agent-card.json` holds a
  card that is not a 0.3 card. `A2aV1Client` and `RemoteA2aV1Agent` choose only interfaces
  for protocol version 1 (`v1_client::v1_interface`).

  Migration: an A2A 0.3 client that reads only `/.well-known/agent-card.json` must read
  `/.well-known/agent.json` instead, or use adk-server's `A2aClient`, which does.
