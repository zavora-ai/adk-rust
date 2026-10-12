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
| `ActingContext` | The `agent` principal, `on_behalf_of`, the `delegation_chain` (outermost first), and the `invocation_id` |

All of them serialize in camelCase.

## The default identity

A run with no configured identity still has one. Each agent acts as
`urn:adk:agent:{app_name}/{agent_name}` (`AgentPrincipal::derived`; characters outside
`[A-Za-z0-9-._~]` are percent-encoded) on behalf of the run's user as a `SubjectKind::Human`.
The same app and agent names always derive the same id, so a restarted process keeps it.

## Configuring a principal

| Where | Setter | Scope |
|-------|--------|-------|
| Runner | `Runner::builder().agent_principal(principal)` | Every run of the runner |
| Runner | `Runner::builder().identity_source(source)` | Every run; the source is asked at the start of each run |
| One run | `RunConfig::builder().agent_principal(principal)` or `.acting_context(acting)` | That run; overrides the runner |

A configured principal applies to every agent in the run, including transfer targets. The
run's user fills `on_behalf_of` unless the configured context names a subject. A run whose
identity source fails ends with the source's error before any agent runs.

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

> **Note:** Put the deployment or replica in `instance`, never in the id. A redeploy that
> keeps the `AgentId` keeps its policy rules, spend history, and audit trail.

## Reading the acting context

`ReadonlyContext::acting_context()` returns the context on every runtime context: invocation,
callback, and tool contexts, workflow wrappers, graph agent nodes, and realtime tools. Graph
function nodes read the configured context through `NodeContext::acting_context()`.

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

`IdentitySource` reports the principal (`current()`) and, optionally, a credential proving it
(`credential()` returns an `AgentCredential` of kind `JwtSvid`, `X509`, or `Opaque`, with its
expiry). `StaticIdentitySource` holds a fixed principal and credential.

The SPIFFE Workload API and cloud metadata servers are extension points: implement the
two-method trait over the client you deploy with, and return the same `AgentId` from every
replica.

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
