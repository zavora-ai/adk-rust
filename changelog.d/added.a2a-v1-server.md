- **`A2aV1Server`** (`adk-server`, feature `a2a-v1`): serves an agent over A2A v1 in one call.
  `A2aV1Server::builder(runner_config, base_url).build()` assembles the task store,
  executor, request handler and agent card. Its card lists the JSON-RPC and HTTP+JSON
  interfaces it mounts, and its skills come from the agent (`v1::card::v1_agent_skills`).
  - `router()` serves the card, with ETag, and both bindings behind `A2A-Version`
    negotiation.
  - `card_router()` and `rpc_router()` serve each part separately, so authentication can
    go on the bindings alone.
  - The builder sets the paths, the task store, the push sender, the card's name,
    description, version and streaming flag, extra skills, and a 0.3 endpoint to list.
  - `v1::card::serve_agent_card` serves any `CachedAgentCard`.
