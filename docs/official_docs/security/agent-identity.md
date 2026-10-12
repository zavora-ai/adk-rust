# Agent Identity

Every run has an acting agent. An `AgentPrincipal` names it with a stable `AgentId`, and an
`ActingContext` adds the subject the agent acts for and the agents that delegated to it. Tool
policy, approvals, spend, and audit records all carry it, so a decision or a dollar is
attributed to the agent that made it — across redeploys, replicas, and delegation.

## Types

| Type | Holds |
|------|-------|
| `AgentId` | A URI such as `spiffe://example.org/agent/billing` or `urn:adk:agent:billing`; non-empty, at most 512 bytes, no control characters. Stable across redeploys |
| `AgentPrincipal` | The `id`, plus optional `tenant`, `class` (template or blueprint), `instance` (deployment or replica), `sponsor`, and free-form `attributes` |
| `SubjectRef` | A `SubjectKind` (`Human`, `Service`, `Agent`) and an id |
| `ActingContext` | The `agent` principal, `on_behalf_of`, and the `delegation_chain` (outermost first) |
| `ActingConfig` | A run's identity configuration: `principals` by agent name, `on_behalf_of`, `subject_kind`, and `delegation_chain` (`RunConfig::acting`) |

All of them serialize in camelCase.

## Identity model

Three identities describe a run. Each answers one question, and each value has exactly one
source.

| Identity | Answers | Fields | Source |
|----------|---------|--------|--------|
| `AdkIdentity` | Whose data | `app_name`, `user_id`, `session_id` | The session the run continues |
| `ExecutionIdentity` | Which execution | `AdkIdentity`, `invocation_id`, `branch`, `agent_name` | The runner, per invocation |
| `ActingContext` | Who acts, for whom, through whom | `agent`, `on_behalf_of`, `delegation_chain` | `RunConfig::acting`, resolved for the executing agent |

The runtime keeps these invariants:

| Invariant | Rule | Enforced by |
|-----------|------|-------------|
| The session user is the subject | `on_behalf_of` is the session's `user_id` as the run's `subject_kind`. A run configured to act for another subject fails at its start with `identity.subject_mismatch` (`Forbidden`) and an `access` audit record, unless the runner's `SubjectDelegationPolicy` allows it; the session's subject then heads the delegation chain | `Runner`, before any agent runs |
| The subject kind is explicit | `SubjectKind::Human` for interactive runs; `SubjectKind::Service` for runs created by `DurableScheduler`, `WebhookIngress`, and `QueueIngress` (`NewRun::with_service_subject`) | `ActingConfig::subject_kind`, the stored principal of a durable run |
| Each agent acts as itself | The executing agent — root, transfer target, workflow or team sub-agent — acts as its entry in `ActingConfig::principals`, else its derived id. A runner-level principal applies to the root agent only; an identity source names other agents through `principal_for` | `Runner`, `LlmAgent`, `CodeActAgent` |
| One invocation id | `ActingContext` holds none; the invocation is `ReadonlyContext::invocation_id()`. Audit records pair the two with `AuditActor::new(&acting, invocation_id)` | The type |
| A stored principal is a claim | `DurableWorker` acts as its own principal unless the run's stored agent id is in `DurableWorkerConfig::allowed_principals` (empty by default), and refuses any other stored agent id with `identity.principal_not_allowed` | `DurableWorker` |

> **Note:** Contexts recorded before the invocation id moved to the execution identity carry an
> `invocationId` field; it is ignored when they are read.

## The default identity

A run with no configured identity still has one. Each agent acts as
`urn:adk:agent:{app_name}/{agent_name}` (`AgentPrincipal::derived`; characters outside
`[A-Za-z0-9-._~]` are percent-encoded) on behalf of the run's user as a `SubjectKind::Human`.
The same app and agent names always derive the same id, so a restarted process keeps it.

## Configuring a principal

| Where | Setter | Scope |
|-------|--------|-------|
| Runner | `Runner::builder().agent_principal(principal)` | The root agent of every run |
| Runner | `Runner::builder().identity_source(source)` | Every run; the root agent acts as `principal_for(root)` or `current()`, every other agent as `principal_for(name)` when the source maps it |
| One run | `RunConfig::builder().agent_principal(agent_name, principal)` or `.acting(config)` | The named agent in that run; overrides the runner |

An agent with no configured principal acts as its derived id, so the sub-agents of a
`SequentialAgent`, `ParallelAgent`, `LoopAgent`, or team are told apart in policy decisions,
spend, and audit records. `StaticIdentitySource::with_agent(name, principal)` maps further
agents. A run whose identity source fails ends with the source's error before any agent runs.

```rust
use std::sync::Arc;

use adk_core::{AgentId, AgentPrincipal, DeclarativePolicy, PolicyRule, StaticIdentitySource};
use adk_runner::Runner;

let principal = AgentPrincipal::new(AgentId::new("spiffe://acme.example/agent/billing")?)
    .with_tenant("acme")
    .with_class("billing-agent")
    .with_instance(std::env::var("HOSTNAME").unwrap_or_default());

let policy = DeclarativePolicy::builder()
    .rule(PolicyRule::allow("refund").for_agent("spiffe://acme.example/agent/billing"))
    .build();

let runner = Runner::builder()
    .app_name("billing")
    .agent(agent)
    .session_service(session_service)
    .tool_policy(Arc::new(policy))
    .identity_source(Arc::new(StaticIdentitySource::new(principal)))
    .build()?;
```

## Acting for another subject

A run acts for its session's user. A service that works for a person — a CRM sync acting for
the account owner, an assistant acting for its principal — asks for the person with
`RunConfig::builder().on_behalf_of(subject)`, and the runner allows it only through its
`SubjectDelegationPolicy`:

```rust
use std::sync::Arc;

use adk_core::{RunConfig, StaticSubjectDelegation, SubjectKind, SubjectRef};
use adk_runner::Runner;

let runner = Runner::builder()
    .app_name("crm")
    .agent(agent)
    .session_service(session_service)
    .subject_delegation(Arc::new(
        StaticSubjectDelegation::new().allow_any(SubjectRef::service("crm-sync")),
    ))
    .build()?;

// A session of the `crm-sync` service, acting for bob.
let config = RunConfig::builder()
    .subject_kind(SubjectKind::Service)
    .on_behalf_of(SubjectRef::human("bob"))
    .build();
```

The run's agents act for `human:bob` with `service:crm-sync` at the head of the delegation
chain. Implement `SubjectDelegationPolicy` to decide per request — over a token exchange or a
directory lookup. Without a policy, or when it answers `false`, the run fails with
`identity.subject_mismatch` before any agent runs, and the refusal is audited.

## Durable runs

A `DurableWorker` runs an agent as the runner's own principal for the root agent, on behalf of
the run's user. A `RunRecord::principal` was written by whoever enqueued the run, so the worker
treats it as a claim:

| Stored principal | Worker |
|------------------|--------|
| None | Acts as its own principal, for the user as `Human` |
| Agent id is the worker's own, or the derived id of the target | Acts as its own principal |
| Agent id is in `DurableWorkerConfig::allowed_principals` (`with_allowed_principal`) | Acts as the stored principal |
| Any other agent id | Fails the run with `identity.principal_not_allowed` and an `access` audit record |
| Subject is the run's user | Acts for the user as the stored subject's kind |
| Subject is anyone else | The runner's `SubjectDelegationPolicy` decides; a delegation chain headed by the run's user gives the user's kind |

`DurableScheduler`, `WebhookIngress`, and `QueueIngress` store the derived principal of the
target acting for the run's user as a `Service`, so their audit records name a service.

Self-approval checks compare ids across the `Human` and `Service` kinds: a person may not
approve a call made by a scheduled run acting for the same id as a service.

> **Note:** Put the deployment or replica in `instance`, never in the id. A redeploy that
> keeps the `AgentId` keeps its policy rules, spend history, and audit trail.

## Reading the acting context

`ReadonlyContext::acting_context()` returns the context on every runtime context: invocation,
callback, and tool contexts, workflow wrappers, graph agent nodes, and realtime tools. Graph
function nodes read the run's `ActingConfig` through `NodeContext::acting()`.

```rust
use std::sync::Arc;

use adk_core::{Result, Tool, ToolContext, async_trait};
use serde_json::{Value, json};

struct WhoAmI;

#[async_trait]
impl Tool for WhoAmI {
    fn name(&self) -> &str {
        "who_am_i"
    }

    fn description(&self) -> &str {
        "Reports the acting agent."
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let acting = ctx.acting_context();
        Ok(json!({ "agent": acting.agent.id.as_str(), "for": acting.on_behalf_of }))
    }
}
```

A wrapper context that implements `ReadonlyContext` must forward `acting_context()` to the
context it wraps.

## Delegation

An `AgentTool` runs its agent as a delegate. The delegate acts for the caller's subject, its
`delegation_chain` gains the caller's `AgentId`, and its principal is:

| `AgentTool` setting | Delegate principal |
|---------------------|--------------------|
| None | `urn:adk:agent:{app}/{wrapped agent}`, in the caller's tenant |
| `.agent_principal(principal)` | `principal` |

## Where the identity is used

| Consumer | What it records or matches |
|----------|----------------------------|
| `ToolPolicyRequest::acting` | Every policy evaluation; `DeclarativePolicy` rules match it with `for_agent(glob)`, `for_tenant(tenant)`, and `for_class(class)` |
| `ToolConfirmationRequest::acting` | Every call held for approval, persisted by every `ApprovalStore` backend |
| `ApprovalRequest::acting` | The request an `ApprovalService` opens for a held call; the requester defaults to the agent's `on_behalf_of` subject, and self-approval checks cover the acting agent |
| `SpendKey::agent` | Model spend (`LlmSpendRecorder`) and payment spend are keyed by the `AgentId` |
| `CommerceActor` | Payment tools report the `AgentId` as `actor_id` and the principal's tenant |
| `AuditRecordDraft::actor` | Every runtime audit record — tool and approval decisions, payments, unknown outcomes — names the agent id, tenant, instance, `on_behalf_of` subject, and delegation chain (`AuditActor::for_context`) |
| `adk_auth::AuditEvent` | `agent_id`, `on_behalf_of`, and `tenant_id` from `with_acting_context`; `ProtectedTool` and `ScopeGuard` set them; `AuditFilter::agent_id` filters on them |
| Tracing | Policy decisions log `agent.id` |

A rule restricted to an agent, tenant, or class does not match a call with no acting context.

## Identity sources

`IdentitySource` reports the root agent's principal (`current()`), the principal of other named
agents (`principal_for(agent_name)`, mapping none by default), and, optionally, a credential
proving the identity (`credential()` returns an `AgentCredential` of kind `JwtSvid`, `X509`, or
`Opaque`, with its expiry). `StaticIdentitySource` holds a fixed principal, fixed principals for
named agents (`with_agent`), and a credential.

The SPIFFE Workload API and cloud metadata servers are extension points: implement the trait
over the client you deploy with (`current()` is the only required method), and return the same
`AgentId` from every replica.

## Credential brokers

A `CredentialBroker` mints a `ScopedToken` for one acting agent, one resource, and a set of
scopes. The resource is an RFC 8707 resource indicator — an absolute `http(s)` URL without a
fragment or user information (`resource_indicator`) — and becomes the token's audience.
`ScopedToken::authorize_for(url)` returns the secret only for a URL with the audience's scheme,
host, and port and a path at or below the audience path; any other URL fails with
`credential.audience_mismatch`, so a token minted for resource A is never sent to resource B.
`revoke(scope)` withdraws tokens for one agent, one resource, both, or all.

| Broker | Crate and feature | Source of the token |
|--------|-------------------|---------------------|
| `SecretCredentialBroker` | `adk-auth` | A `SecretProvider` secret bound to an exact `(AgentId, resource)` pair with its granted scopes |
| `TokenExchangeBroker` | `adk-auth`, `token-exchange` | An RFC 8693 token exchange of the identity source's credential at an authorization server; optional RFC 7009 revocation |

```rust
use std::sync::Arc;

use adk_auth::SecretCredentialBroker;
use adk_core::{AgentId, resource_indicator};

let broker = SecretCredentialBroker::new(secret_provider)
    .bind(
        AgentId::new("spiffe://acme.example/agent/billing")?,
        resource_indicator("https://ledger.acme.example/mcp")?,
        "billing-ledger-token",
        vec!["ledger.read".to_string()],
    )
    .with_ttl(std::time::Duration::from_secs(300));
```

### Outbound connections

| Connection | Setter | Token minted for |
|------------|--------|------------------|
| MCP over streamable HTTP | `McpHttpClientBuilder::with_credential_broker(broker, acting, scopes)` | The acting context passed to the builder, at the MCP endpoint; checked before every request |
| A2A remote agent | `RemoteA2aAgent::builder(name).credential_broker(broker, scopes)` | The run's acting context, at the configured agent URL |
| A2A client | `A2aClient::with_credential(token)`, `A2aV1Client::with_credential(token)` | The token's audience; requests outside it fail without being sent |

## Error codes

| Code | Category | Meaning |
|------|----------|---------|
| `identity.agent_id.invalid` | `InvalidInput` | The id is empty, longer than 512 bytes, or contains a control character |
| `identity.subject_mismatch` | `Forbidden` | The run asks to act for a subject other than its session's user, and no `SubjectDelegationPolicy` allows it |
| `identity.principal_not_allowed` | `Forbidden` | A durable run's stored principal names an agent the worker may not act as |
| `identity.credential.unavailable` | `Unsupported` | The identity source issues no credential |
| `identity.credential.subject_mismatch` | `Internal` | The configured credential identifies a different agent |
| `identity.credential.expired` | `Unauthorized` | The configured credential has expired |
| `credential.resource.invalid` | `InvalidInput` | The resource is not an absolute `http(s)` URL without fragment or user information |
| `credential.audience_mismatch` | `Forbidden` | A token was presented outside its audience |
| `credential.expired` | `Unauthorized` | The token has expired |
| `credential.not_bound` | `Forbidden` | `SecretCredentialBroker` has no binding for the agent and resource |
| `credential.scope_exceeded` | `Forbidden` | The requested scopes exceed the binding's grant |
| `credential.subject_mismatch` | `Forbidden` | `TokenExchangeBroker`'s credential identifies a different agent |
| `credential.exchange_unsupported` | `Unsupported` | `TokenExchangeBroker` cannot exchange an `X509` credential |
| `credential.exchange_failed` | `Unavailable` or `Internal` | The authorization server refused the exchange or returned an unreadable response; the error carries the upstream status |
| `credential.revocation_failed` | `Unavailable` | The RFC 7009 revocation endpoint refused a token |

## Related

- [Tool Authorization](tool-authorization.md) — policy rules keyed by agent
- [Spend Ledger](spend-ledger.md) — spend keyed by agent id
- [Access Control](access-control.md) — audit records
- [Durable Stores](durable-stores.md) — approval stores that persist the acting context
