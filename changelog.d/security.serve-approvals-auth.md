- **A served harness requires an authenticated caller** (`adk-cli`): `adk-rust serve` admits
  loopback peers as the operator (`LoopbackOperatorExtractor`, which reads the peer address,
  so `Launcher::run_serve_directly` now serves with connection info) and refuses any other
  `--host` unless `--auth-token` or `ADK_SERVE_TOKEN` sets a bearer token, compared in
  constant time (`BearerTokenExtractor`). `--mode auto` beyond loopback also needs `--sandbox`.
