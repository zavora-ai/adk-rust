- **Sandbox network access is one fail-closed mode** (`adk-sandbox`): `SandboxPolicy`'s
  `allow_network` and `network_rules` become `network: NetworkAccess` — `Denied` (default),
  `Proxied { endpoint }` (only an egress proxy's loopback port, with the standard proxy
  variables set) or `Open`; `allow_domain` and `NetworkRule`, which no enforcer applied, are
  removed, and 2.x policies deserialize unchanged in meaning. `SandboxEnforcer` gains
  `network_support()`, and an enforcer refuses a mode it cannot enforce with
  `SandboxError::Unenforceable` instead of warning and running: Seatbelt enforces all three,
  `Proxied` as an outbound grant to `localhost:<port>` only; bubblewrap enforces `Denied` and
  `Open`, and under `Denied` refuses grants of `/run` paths holding sockets and of `/tmp`,
  `/var/tmp` and `/dev/shm`, and covers them with empty read-only `tmpfs` mounts below a
  granted `/`; the Windows restricted-token enforcer refuses `Denied` and `Proxied` unless built
  with `acknowledge_open_network()`, which the harness shell and `ExternalGenerator` (with
  `with_restricted_token`) set. Refs #1281.
