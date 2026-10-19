- **MCP OAuth token requests require HTTPS** (`adk-tool`): the client-credentials request
  carries the client secret and returns the access token, and `OAuth2Config` sent it to a
  plain-`http` `token_url` as readily as to an `https` one. A token URL whose scheme is not
  `https` now fails with `AuthError::TokenFetch` unless it is `http` on `localhost` or a
  loopback address.
