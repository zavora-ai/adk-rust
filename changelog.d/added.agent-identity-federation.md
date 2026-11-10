- **Agent identity federation with AWS, Google Cloud and Microsoft Entra** (`adk-auth`, feature
  `agent-federation`): `federation::AgentTokenIssuer` is an OpenID Connect issuer for agents —
  a discovery document, a JWK set, and RS256 tokens whose `sub` is the agent's `AgentId`,
  valid for 10 minutes by default and 1 hour at most. Signing goes through
  `AgentTokenSigner`; `RsaTokenSigner` holds a PKCS#8 RSA key that is never printed.
  `AwsWebIdentity` (`sts:AssumeRoleWithWebIdentity`), `AgentCoreIdentity`
  (`bedrock-agentcore:GetWorkloadAccessTokenForJWT`, Signature Version 4),
  `GoogleWorkloadIdentity` (Security Token Service) and `EntraAgentIdentity` (the blueprint and
  agent identity exchanges of Entra Agent ID) exchange the agent's token for the cloud's
  credential; the Google and Entra clients are `CredentialBroker`s. `InboundAgentVerifier`
  validates a Microsoft Entra or Google Cloud agent token with the `sso` validators and names
  the agent as an `AgentPrincipal` through an `AgentClaimsProfile` (`EntraAgentProfile`,
  `GoogleAgentProfile`). `FederationError` converts into `AdkError` with
  `identity.federation.*` codes.
