- **Agent identity through every run** (`adk-core`, `adk-runner`, `adk-agent`, `adk-tool`,
  `adk-graph`, `adk-realtime`): every run has an acting agent, an `ActingContext` — the
  agent's `AgentPrincipal` (an `AgentId` plus tenant, class, instance, sponsor, and
  attributes), the `SubjectRef` it acts for, and the delegation chain. The types live in
  `adk_core::principal`. `ReadonlyContext::acting_context()` exposes the context on every
  runtime context, and `NodeContext::acting()` gives graph nodes the run's `ActingConfig`.
  Each agent — root, transfer target, workflow or team sub-agent — acts as itself: its
  principal comes from `RunConfig::acting` (`ActingConfig::principals` by agent name,
  `RunConfigBuilder::agent_principal(name, principal)`), the runner's
  `identity_source(...)` (the root agent as `IdentitySource::current`, any agent as
  `IdentitySource::principal_for`), or `AgentTool::agent_principal`; otherwise it acts as
  `urn:adk:agent:{app}/{agent}` (`AgentPrincipal::derived`, `ActingContext::derived`).
  `Runner::builder().agent_principal(...)` names the root agent only. `AgentTool`
  delegation appends the caller to the delegate's chain (`ActingContext::delegate`).
  `PrincipalError` converts into `AdkError` with code `identity.agent_id.invalid`.
- **A run acts for its session's user** (`adk-core`, `adk-runner`, `adk-server`): the
  subject is the session's user as `ActingConfig::subject_kind` — `Human` by default,
  `Service` for runs that `DurableScheduler`, `WebhookIngress`, and `QueueIngress` create
  (`NewRun::with_service_subject`). A run configured to act for anyone else
  (`RunConfigBuilder::on_behalf_of`) fails at its start with `identity.subject_mismatch`
  (`Forbidden`), recorded as an `access` audit record when the run has an audit trail, unless
  a `SubjectDelegationPolicy` (`StaticSubjectDelegation`) set with
  `Runner::builder().subject_delegation(...)` allows it; the session's subject then heads the
  delegation chain.
- **Identity-keyed policy and approvals** (`adk-core`): `ToolPolicyRequest::acting` and
  `ToolConfirmationRequest::acting` carry the acting context, and `PolicyRule::for_agent`,
  `for_tenant`, and `for_class` restrict a rule to it. The governed path passes it to
  `ApprovalService::submit_call`, so `ApprovalRequest::acting` and the requester are set for
  every held call, and `ApprovalRequest::is_self` treats a human and a service with the same
  id as one party.
- **Identity sources and credential brokers** (`adk-core`, `adk-auth`): `IdentitySource`
  with `StaticIdentitySource` (`with_agent` maps named agents), `AgentCredential`
  (`CredentialKind::JwtSvid`, `X509`, `Opaque`), and `CredentialBroker` minting
  audience-bound `ScopedToken`s (RFC 8707) that `authorize_for` refuses outside their
  audience. `SecretCredentialBroker` serves secrets bound to one agent and resource;
  `TokenExchangeBroker` (`token-exchange` feature) performs RFC 8693 token exchange with
  optional RFC 7009 revocation, gives up after 30 seconds (5-second connect timeout), and
  sends one exchange for concurrent `token_for` calls of an uncached token.
- **Brokered outbound auth** (`adk-tool`, `adk-server`):
  `McpHttpClientBuilder::with_credential_broker`, `RemoteA2aAgentBuilder::credential_broker`,
  and `A2aClient::with_credential` / `A2aV1Client::with_credential` attach a bearer only to
  requests inside the token's audience.
- **Audit records carry the acting agent** (`adk-core`, `adk-runner`, `adk-auth`): every
  `AuditRecordDraft` the runtime writes names the acting agent id, tenant, instance,
  `on_behalf_of` subject, and delegation chain (`AuditActor::new`, `AuditActor::for_context`,
  `ToolGate::with_acting_context`). `adk-auth`'s `AuditEvent::agent_id` and `on_behalf_of`
  are filled by `ProtectedTool` and `ScopeGuard` (`AuditEvent::with_acting_context`),
  filterable with `AuditFilter::agent_id`, and stored by the PostgreSQL and OTLP sinks.
- **Durable identity checks** (`adk-server`, `adk-runner`): `DurableWorker` acts as the
  runner's principal and honours a run's stored `RunRecord::principal` only when it names
  the worker's own or the target's derived id, or an id in
  `DurableWorkerConfig::allowed_principals` (`with_allowed_principal`; empty by default);
  any other fails the run with `identity.principal_not_allowed`, recorded through
  `DurableWorker::with_audit`. `Runner::default_principal`, `Runner::root_agent_name`,
  `Runner::audit_trail`, and `Runner::recover_unanswered_calls_with_config` support the
  checks.
