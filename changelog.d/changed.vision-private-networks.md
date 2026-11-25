- **Vision images may go to a model on a private network** (`adk-vision`, `adk-harness`,
  `adk-cli`): the harness vision pack and `adk-rust vision` build their egress gate with
  `EgressPolicy::allow_private_networks()`, so a self-hosted model on `localhost` or the local
  network receives images like a cloud host. `private_networks = false` in the `[egress]` table
  of `.adk/vision.toml` (`EgressConfig::public_networks_only`) keeps image egress to public
  addresses; the gate's policy stays the ceiling the switch narrows, and cloud metadata
  endpoints are refused either way.
