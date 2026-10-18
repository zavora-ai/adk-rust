- **Identity-provider requests time out, and concurrent token exchanges share one
  request** (`adk-auth`): `JwksCache` refreshes and `OidcProvider::from_discovery` give
  up after 10 seconds and `TokenExchangeBroker` requests after 30, each with a 5-second
  connect timeout; a provider that never answered held the caller, and the JWKS refresh
  lock, indefinitely. Concurrent `token_for` calls for a token that is not cached send
  one exchange instead of one each. `JwksCache::with_http_client` replaces its client.
