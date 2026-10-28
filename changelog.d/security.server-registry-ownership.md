- **The agent registry authenticates callers and enforces card ownership** (`adk-server`,
  feature `agent-registry`): the registry routes accepted any non-empty `Authorization`
  header and checked no identity, so any client could register, overwrite (by deleting and
  registering again), and delete every card. `registry_router` now takes a
  `RequestContextExtractor` and authenticates every request through it (`401` for a refused
  token or an empty `user_id`). `AgentCard` gains `owner`, set from the caller; any
  authenticated caller lists and reads every card, a registered name is a `409` for everyone,
  and only the owner updates the card through the new `PUT /agents/{name}` or deletes it —
  anyone else gets `403`. A card stored without an owner stays readable and changes only
  through the store. Callers pass an extractor to `registry_router(store, extractor)`,
  `AgentCard` literals set `owner`, and an `AgentRegistryStore` implements the atomic
  `replace_owned` and `delete_owned` (returning `OwnerWrite`).
