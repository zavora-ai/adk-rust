- **Agent identity** (`adk-core`, `adk-runner`, `adk-agent`, `adk-tool`, `adk-graph`,
  `adk-realtime`, `adk-auth`, `adk-server`): every run has an acting agent. `AgentId`,
  `AgentPrincipal`, `SubjectRef`, and `ActingContext` describe it;
  `ReadonlyContext::acting_context()` exposes it on every runtime context and
  `NodeContext::acting_context()` on graph nodes. Configure it with
  `Runner::builder().identity_source(...)` or `.agent_principal(...)`, `RunConfig::acting_context`,
  or `AgentTool::agent_principal`; otherwise each agent acts as `urn:adk:agent:{app}/{agent}`
  for the run's user. `AgentTool` delegation appends the caller to the delegate's chain.
- **Identity-keyed policy** (`adk-core`): `PolicyRule::for_agent`, `for_tenant`, and `for_class`
  restrict a rule to the acting agent.
- **Identity sources and credential brokers** (`adk-core`, `adk-auth`): `IdentitySource` with
  `StaticIdentitySource`, `AgentCredential` (`JwtSvid`, `X509`, `Opaque`), and `CredentialBroker`
  minting audience-bound `ScopedToken`s (RFC 8707) that `authorize_for` refuses outside their
  audience. `SecretCredentialBroker` serves secrets bound to an agent and resource;
  `TokenExchangeBroker` (`token-exchange` feature) performs RFC 8693 token exchange with
  optional RFC 7009 revocation.
- **Brokered outbound auth** (`adk-tool`, `adk-server`):
  `McpHttpClientBuilder::with_credential_broker`, `RemoteA2aAgentBuilder::credential_broker`, and
  `A2aClient::with_credential` / `A2aV1Client::with_credential` attach a bearer only to requests
  inside the token's audience.
- **Audit identity** (`adk-auth`): `AuditEvent::agent_id` and `on_behalf_of`, set by
  `with_acting_context` in `ProtectedTool` and `ScopeGuard`, filterable with
  `AuditFilter::agent_id`, and stored by the PostgreSQL and OTLP sinks.
- **`agent_identity` scenario** (`examples/autonomy_validation`): live check that policy
  decisions and spend records carry the configured agent id across two deployments.
