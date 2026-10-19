- **A2A clients send messages only to the origin the agent card came from** (`adk-server`): a
  card could name any URL as its endpoint, so a hostile or compromised card redirected every
  message to a host of its choosing. `A2aClient::resolve_agent_card`, `A2aClient::from_url`,
  `A2aV1Client::resolve_agent_card`, `A2aV1Client::resolve_agent_card_cached`, and
  `RemoteA2aAgent` refuse a card whose endpoint is on another origin with
  `a2a.card_endpoint_refused`; the v1 client returns it in the new
  `V1ClientError::CardEndpoint` variant. `localhost`, `127.0.0.1`, and `::1` count as one
  host. `A2aClient::resolve_agent_card_allowing`, `A2aV1Client::resolve_agent_card_allowing`,
  and `RemoteA2aAgentBuilder::allow_endpoint_origin` accept further origins, over `https`
  unless loopback, and `check_card_endpoint` applies the same rule elsewhere.
  `RemoteA2aConfig` gains a public `allowed_endpoint_origins` field, so struct literals must
  add it.

  Migration: when a card legitimately names its endpoint on another origin, pass that origin to
  `allow_endpoint_origin` or `resolve_agent_card_allowing`.
