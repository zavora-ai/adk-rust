- **One identity model: `RunConfig::acting` replaces `RunConfig::acting_context`**
  (`adk-core`, `adk-runner`, `adk-tool`, `adk-graph`): `ActingConfig` holds per-agent
  `principals`, `on_behalf_of`, `subject_kind`, and `delegation_chain`.
  `RunConfigBuilder::agent_principal` takes the agent name (`agent_principal(name, principal)`);
  `RunConfigBuilder::acting_context` is replaced by `acting`, `on_behalf_of`, and
  `subject_kind`; `NodeContext::acting_context` is replaced by `NodeContext::acting`.
- **`ActingContext` no longer carries an invocation id** (`adk-core`): the execution
  identity's `invocation_id` is the only one. `ActingContext::new(agent)`,
  `ActingContext::derived(app, agent, user, kind)`, `ActingContext::delegate(principal)`, and
  `RunConfig::acting_context_for(app, agent, user)` drop the invocation argument;
  `AuditActor::new(&acting, invocation_id)` replaces `From<&ActingContext> for AuditActor`.
  Stored contexts with an `invocationId` field still deserialize.
- **A run acts for its session's user** (`adk-runner`): a run configured to act for any other
  subject fails at its start with `identity.subject_mismatch` (`Forbidden`) and an `access`
  audit record, unless a `SubjectDelegationPolicy` set with
  `Runner::builder().subject_delegation(...)` allows it.
- **Runner-level principals apply to the root agent only** (`adk-runner`, `adk-agent`):
  `Runner::builder().agent_principal(...)` and an `IdentitySource`'s `current()` name the root
  agent; transfer targets and workflow or team sub-agents act as `IdentitySource::principal_for`
  their name, else their derived `urn:adk:agent:{app}/{agent}` id.
- **`DurableWorker` refuses untrusted stored principals** (`adk-server`): a run whose
  `RunRecord::principal` names an agent other than the worker's own or the target's derived id
  fails with `identity.principal_not_allowed` unless `DurableWorkerConfig::allowed_principals`
  lists it; the refusal is audited.
