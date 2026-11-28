- **jsonwebtoken 12, and no jsonwebtoken types in the `adk-auth` API** (`adk-auth`, `sso`):
  `JwtValidatorBuilder::algorithm` takes the ADK-owned `JwtAlgorithm` (`RS256`, `RS384`,
  `RS512`, `PS256`, `PS384`, `PS512`, `ES256`, `ES384`; `FromStr` and `Display` use the JWS
  names) instead of `jsonwebtoken::Algorithm`, so a symmetric algorithm, EdDSA or `none` cannot
  be configured. `TokenError` no longer implements `From<jsonwebtoken::errors::Error>`, and
  `JwksCache::get_key`, which returned `jsonwebtoken::DecodingKey`, is crate-private. A later
  jsonwebtoken major is therefore not an `adk-auth` breaking change. Replace
  `.algorithm(jsonwebtoken::Algorithm::ES256)` with `.algorithm(JwtAlgorithm::ES256)` or
  `.algorithm("ES256".parse()?)`, map jsonwebtoken errors to a `TokenError` variant yourself,
  and validate with `JwtValidator` or a provider instead of `get_key`. `adk-surface` (`teams`)
  also moves to jsonwebtoken 12; both stay on `aws-lc-rs`.
