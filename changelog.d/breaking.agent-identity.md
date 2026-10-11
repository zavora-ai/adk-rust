- **Agent identity fields** (`adk-core`, `adk-runner`, `adk-auth`, `adk-server`): struct
  literals need the new fields — `ToolPolicyRequest::acting`, `ToolConfirmationRequest::acting`
  (or use `ToolConfirmationRequest::new`), `RunConfig::acting_context`,
  `RunnerConfig::identity_source`, `AuditEvent::agent_id` and `on_behalf_of`, and
  `RemoteA2aConfig` / `RemoteA2aV1Config::credential_broker` and `credential_scopes`.
  `V1ClientError` gains a `Credential` variant. Context wrappers that implement
  `ReadonlyContext` must forward the new `acting_context()` method, or they report the derived
  identity.
- **Spend and payments are keyed by agent id** (`adk-runner`, `adk-payments`): `SpendKey::agent`
  for model and payment spend, and the payment tools' `CommerceActor::actor_id`, hold the acting
  agent's `AgentId` (`urn:adk:agent:{app}/{agent}` unless configured) instead of the agent name.
  Update agent-scoped `SpendLimits` and spend queries accordingly.
