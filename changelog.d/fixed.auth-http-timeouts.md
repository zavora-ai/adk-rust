- **Identity-provider requests time out** (`adk-auth`, feature `sso`): `JwksCache`
  refreshes and `OidcProvider::from_discovery` give up after 10 seconds
  (`JwksCache::DEFAULT_REQUEST_TIMEOUT`), with a 5-second connect timeout; a provider that
  never answered held the caller, and the JWKS refresh lock, indefinitely.
  `JwksCache::with_http_client` replaces its client.
