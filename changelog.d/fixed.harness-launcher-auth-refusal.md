- **`HarnessLauncher` names its own authentication setting** (`adk-cli`): serving a launcher
  app beyond loopback without a token or extractor is refused with
  `HarnessLauncher::with_auth_token` and `HarnessLauncher::with_request_context_extractor` in
  the message, instead of `--auth-token` and `ADK_SERVE_TOKEN`, which an app's `up` and
  `serve` do not take. `adk-rust serve` and `adk-rust up` keep naming their flags.
