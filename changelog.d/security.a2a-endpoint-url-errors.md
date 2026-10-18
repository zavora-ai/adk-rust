- **A2A clients no longer repeat an endpoint URL in their errors** (`adk-server`): when a
  credentialed `A2aClient` or `v1_client` could not parse the agent card's endpoint URL, the
  error quoted the whole URL, so credentials embedded in it (`https://user:pass@host`) reached
  error messages and logs. The error now names the parse failure only.
