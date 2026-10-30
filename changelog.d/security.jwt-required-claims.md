- **SSO validators require the `iss` and `aud` claims** (`adk-auth`, `sso`): a token signed
  by the configured JWKS with no `aud` claim, or with no `iss` claim, validated, because
  jsonwebtoken checks those claims only when a token carries them. `JwtValidator` and the
  Google, Azure AD, Okta, Auth0, and generic OIDC providers now refuse such a token with
  `TokenError::MissingClaim` (`auth.missing_claim`). `AzureADProvider::multi_tenant` requires
  the `iss` claim and still checks no issuer value.
- **`JwtValidatorBuilder::build` requires an audience** (`adk-auth`, `sso`): a builder with no
  `audience(...)` fails. `without_audience()` opts out for an issuer whose tokens carry no
  `aud` claim; that validator refuses any token that carries an audience and still requires
  `iss`. A token missing `exp` now fails with `TokenError::MissingClaim` instead of
  `TokenError::DecodingError`.
