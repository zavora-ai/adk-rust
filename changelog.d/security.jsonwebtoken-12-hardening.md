- **JWT validation refuses unknown critical headers and mistyped claims** (`adk-auth`, `sso`;
  `adk-surface`, `teams`): on jsonwebtoken 12, a token whose `crit` header names an extension
  the validator does not implement is refused, and an `iss`, `aud` or `sub` claim of the wrong
  JSON type fails validation; jsonwebtoken 11 skipped the issuer and audience comparison for
  such a claim. Tests cover expired and not-yet-valid tokens, another issuer, `alg: none`, HS256
  keyed with the public JWK, a forged signature and an unknown `crit` header for `JwtValidator`
  and every SSO provider.
