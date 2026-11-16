- **A2A v1 wire types from `a2a-protocol-types` 0.15** (`adk-server`): `adk_server::a2a::v1`
  (`convert`, `TaskStoreEntry`, the client, executor and handlers) takes and returns
  `a2a-protocol-types` 0.15 types instead of 0.12. An application that builds A2A messages,
  tasks or agent cards itself depends on `a2a-protocol-types = "0.15"`. The JSON wire format is
  unchanged.
