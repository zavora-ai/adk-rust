- **`Proxied` network access on bubblewrap, and one egress proxy per call** (`adk-sandbox`,
  feature `egress-proxy`): `EgressProxyBuilder::start_for(enforcer)` starts a call's proxy where
  the enforcer needs it (`SandboxEnforcer::proxy_listener`) and returns a `CallProxy` whose
  `network()` the call's policy runs under. Seatbelt reaches it on the host's loopback;
  bubblewrap keeps `--unshare-net`, reports the sandbox's process id on `--info-fd` and holds
  the program on `--block-fd` while a helper joins the sandbox's user and network namespaces and
  opens the proxy's listener on its loopback. The PR tier's macOS job now runs the Seatbelt
  enforcement tests, and `Sandbox Linux` runs `adk-sandbox` with `egress-proxy`. Refs #1281.
