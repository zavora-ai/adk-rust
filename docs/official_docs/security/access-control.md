# Access Control

Enterprise-grade access control for AI agents using `adk-auth`.

## Overview

`adk-auth` provides role-based access control (RBAC), scope-based authorization, audit logging, and SSO support for ADK agents. It enables secure, fine-grained control over which users can access which tools.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        Agent Request                             │
└─────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                     SSO Token Validation                         │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────────┐  │
│  │ Google      │  │ Azure AD    │  │ OIDC Discovery          │  │
│  │ Provider    │  │ Provider    │  │ (Okta, Auth0, etc)     │  │
│  └─────────────┘  └─────────────┘  └─────────────────────────┘  │
│                          │                                       │
│                   ┌──────┴──────┐                                │
│                   │ JWKS Cache  │  ← Auto-refresh keys          │
│                   └─────────────┘                                │
└─────────────────────────────────────────────────────────────────┘
                                │
                                ▼ TokenClaims
┌─────────────────────────────────────────────────────────────────┐
│                       Claims Mapper                              │
│                                                                  │
│    IdP Groups          →        adk-auth Roles                  │
│    ─────────────────────────────────────────                    │
│    "AdminGroup"        →        "admin"                         │
│    "DataAnalysts"      →        "analyst"                       │
│    (default)           →        "viewer"                        │
└─────────────────────────────────────────────────────────────────┘
                                │
                                ▼ Roles
┌─────────────────────────────────────────────────────────────────┐
│                      Access Control                              │
│                                                                  │
│    Role: admin                                                   │
│    ├── allow: AllTools                                          │
│    └── allow: AllAgents                                         │
│                                                                  │
│    Role: analyst                                                 │
│    ├── allow: Tool("search")                                    │
│    ├── allow: Tool("summarize")                                 │
│    └── deny:  Tool("code_exec")  ← Deny takes precedence        │
└─────────────────────────────────────────────────────────────────┘
                                │
                                ▼ Check Result
┌─────────────────────────────────────────────────────────────────┐
│                      Audit Logging                               │
│                                                                  │
│    {"user":"alice","resource":"search","outcome":"allowed"}     │
│    {"user":"bob","resource":"exec","outcome":"denied"}          │
└─────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                     Tool Execution                               │
│               (only if access granted)                          │
└─────────────────────────────────────────────────────────────────┘
```

## Design Principles

### 1. Deny Precedence

When a role has both allow and deny rules, **deny always wins**:

```rust
use adk_auth::{Permission, Role};

let role = Role::new("limited")
    .allow(Permission::AllTools)               // Allow everything...
    .deny(Permission::Tool("admin".into()));   // ...except admin

// Result: Can access any tool EXCEPT "admin"
```

### 2. Multi-Role Union

Users with multiple roles get the **union** of permissions, but deny rules from any role still apply:

```rust
use adk_auth::{AccessControl, Permission, Role};

let ac = AccessControl::builder()
    .role(Role::new("reader").allow(Permission::tool("search")))
    .role(Role::new("writer").allow(Permission::tool("write")))
    .assign("alice", "reader")
    .assign("alice", "writer")
    .build()?;

// Alice can access both "search" AND "write"
assert!(ac.check("alice", &Permission::tool("search")).is_ok());
assert!(ac.check("alice", &Permission::tool("write")).is_ok());
```

`build()` fails with `AuthError::RoleNotFound` when a user is assigned a role that was not added.

### 3. Explicit Over Implicit

Permissions are explicit - no access is granted by default:

```rust
use adk_auth::{AccessControl, Permission, Role};

let ac = AccessControl::builder()
    .role(Role::new("empty")) // This role grants NO permissions
    .assign("user", "empty")
    .build()?;

assert!(ac.check("user", &Permission::tool("anything")).is_err()); // → Denied
assert!(ac.check("unknown-user", &Permission::tool("anything")).is_err()); // no roles → Denied
```

### 4. Separation of Authentication and Authorization

- **Authentication** (SSO): Verifies WHO the user is (via JWT)
- **Authorization** (RBAC): Determines WHAT they can access

```rust
use adk_auth::Permission;
use adk_auth::sso::TokenValidator;

// Authentication: validate JWT, extract claims
let claims = provider.validate(token).await?;

// Authorization: check specific permission
ac.check(&claims.sub, &Permission::tool("search"))?;

// Combined: SsoAccessControl does both
sso.check_token(token, &Permission::tool("search")).await?;
```

## Installation

```toml
[dependencies]
adk-auth = "3.0.0"

# For SSO/OAuth support
adk-auth = { version = "3.0.0", features = ["sso"] }
```

## Core Components

### Permission

```rust
pub enum Permission {
    Tool(String),     // Specific tool by name
    AllTools,         // Wildcard: all tools
    Agent(String),    // Specific agent by name
    AllAgents,        // Wildcard: all agents
}
```

`Permission::tool(name)` and `Permission::agent(name)` build the named variants from any
`impl Into<String>`.

### Role

```rust
use adk_auth::{Permission, Role};

let analyst = Role::new("analyst")
    .allow(Permission::Tool("search".into()))
    .allow(Permission::Tool("summarize".into()))
    .deny(Permission::Tool("code_exec".into()));
```

### AccessControl

```rust
use adk_auth::{AccessControl, Permission};

let ac = AccessControl::builder()
    .role(admin)
    .role(analyst)
    .assign("alice@company.com", "admin")
    .assign("bob@company.com", "analyst")
    .build()?;

// Check permission
ac.check("bob@company.com", &Permission::Tool("search".into()))?;
```

### ProtectedTool

Wraps a tool with automatic permission checking. The check runs inside `execute()` against
`Permission::Tool(<tool name>)` for the context's user; a denial fails the call with a tool
error and the wrapped tool does not run. `ProtectedTool` and the `ScopeGuard` wrappers report
the wrapped tool's name, schema, effect, origin, and annotations unchanged, so policy and
approvals see an MCP tool's hints and declaration digest through the wrapper.

```rust
use adk_auth::ToolExt;
use adk_core::Tool;
use std::sync::Arc;

let protected = my_tool.with_access_control(Arc::new(ac));

// When executed, checks permission before running
protected.execute(ctx, args).await?;
```

### AuthMiddleware

Batch-protect multiple tools:

```rust
use adk_auth::AuthMiddleware;

// tools: Vec<Arc<dyn Tool>>
let middleware = AuthMiddleware::new(ac);
let protected_tools = middleware.protect_all(tools);
```

### ScopeGuard

Use scopes for request-level authorization that comes from JWT claims or session state. A
tool declares the scopes it needs with `Tool::required_scopes()`; `ScopeGuard` refuses a call
unless the resolver grants every one of them, and passes through a tool that declares none.
`ContextScopeResolver` reads `ToolContext::user_scopes()`:

```rust
use adk_auth::{ContextScopeResolver, ScopeGuard};

let guard = ScopeGuard::new(ContextScopeResolver);
let protected = guard.protect(my_tool);
```

### Combining RBAC + Scopes

RBAC answers "may this user access the tool at all?" Scopes answer "is this specific request authorized right now?"

```rust
use std::sync::Arc;
use adk_auth::{AuthMiddleware, ContextScopeResolver, ScopeGuard};

let rbac = AuthMiddleware::new(ac);
let scoped = ScopeGuard::new(ContextScopeResolver);

let protected = scoped.protect(rbac.protect(transfer_tool));
```

## SSO Integration

### Supported Providers

| Provider | Constructor | Issuer |
|----------|-------------|--------|
| Google | `GoogleProvider::new(client_id)` | accounts.google.com |
| Azure AD | `AzureADProvider::new(tenant, client)` or `AzureADProvider::multi_tenant(client).with_allowed_tenants(["tenant-id"])` | login.microsoftonline.com |
| Okta | `OktaProvider::new(domain, client)` | {domain}/oauth2/default |
| Auth0 | `Auth0Provider::new(domain, audience)` | {domain}/ |
| Generic | `OidcProvider::from_discovery(issuer, client).await` | Any OIDC provider |
| Custom JWT | `JwtValidator::builder().issuer(iss).audience(aud).jwks_uri(uri).build()?` | Any issuer that publishes a JWKS |

`AzureADProvider::multi_tenant()` accepts any tenant for the configured audience unless you explicitly restrict it with `with_allowed_tenants(...)`.

### Required claims

Every validator requires the `exp`, `iss`, and `aud` claims. A token without one of them fails
with `TokenError::MissingClaim` (`auth.missing_claim`); a token that names another issuer or
audience fails with `TokenError::ValidationError` (`auth.validation`).

| Validator | `iss` must be | `aud` must name |
|-----------|---------------|-----------------|
| `GoogleProvider` | `https://accounts.google.com` or `accounts.google.com` | The client ID |
| `AzureADProvider::new` | `https://login.microsoftonline.com/{tenant}/v2.0` | The client ID |
| `AzureADProvider::multi_tenant` | Present; the value is not checked, so restrict tenants with `with_allowed_tenants(...)` | The client ID |
| `OktaProvider` | `https://{domain}/oauth2/{server}` | The client ID |
| `Auth0Provider` | `https://{domain}/` | The audience |
| `OidcProvider` | The configured issuer | The client ID |
| `JwtValidator` | The configured issuer | The configured audience |

`JwtValidatorBuilder::build` fails unless the builder sets an audience. For an issuer whose
tokens carry no `aud` claim, `without_audience()` opts out: the validator then accepts a token
with no `aud` claim, refuses one that carries any audience, and still requires `iss`. A token
that names no audience does not say which client it was minted for, so pin an audience
whenever the issuer sets one.

```rust
use adk_auth::sso::{JwtValidator, TokenError};

fn validators() -> Result<(JwtValidator, JwtValidator), TokenError> {
    let pinned = JwtValidator::builder()
        .issuer("https://issuer.example.com")
        .audience("https://api.example.com")
        .jwks_uri("https://issuer.example.com/.well-known/jwks.json")
        .build()?;
    // Only for an issuer that never puts `aud` in its tokens.
    let unpinned = JwtValidator::builder()
        .issuer("https://issuer.example.com")
        .jwks_uri("https://issuer.example.com/.well-known/jwks.json")
        .without_audience()
        .build()?;
    Ok((pinned, unpinned))
}
```

OIDC discovery and every JWKS refresh give up after 10 seconds (connect timeout 5 seconds), so an identity provider that accepts the connection and never answers fails the validation instead of holding it and the JWKS refresh lock. `JwksCache::with_http_client` replaces the client.

### TokenClaims

Claims extracted from validated JWTs (abridged):

```rust
pub struct TokenClaims {
    pub sub: String,                  // Subject (user ID)
    pub iss: String,                  // Issuer
    pub email: Option<String>,        // Email
    pub email_verified: Option<bool>, // Whether the IdP verified the email
    pub name: Option<String>,         // Display name
    pub groups: Vec<String>,          // IdP groups
    pub roles: Vec<String>,           // IdP roles
    pub hd: Option<String>,           // Google hosted domain
    pub tid: Option<String>,          // Azure tenant ID
    // ... aud, exp, iat, nbf, jti, given_name, family_name,
    //     preferred_username, picture, and custom claims
}
```

### ClaimsMapper

Maps IdP groups to adk-auth roles:

```rust
use adk_auth::sso::ClaimsMapper;

let mapper = ClaimsMapper::builder()
    .map_group("AdminGroup", "admin")
    .map_group("Users", "viewer")
    .default_role("guest")
    .user_id_from_email()
    .build();
```

`user_id_from_email()` only uses the email claim when `email_verified == true`; otherwise it falls back to `sub`.

### SsoAccessControl

Combines SSO validation with RBAC in one call:

```rust
use adk_auth::Permission;
use adk_auth::sso::{GoogleProvider, SsoAccessControl};

let sso = SsoAccessControl::builder()
    .validator(GoogleProvider::new("client-id"))
    .mapper(mapper)
    .access_control(ac)
    .audit_sink(audit)
    .build()?;

// Validate token + check permission + audit log
let claims = sso.check_token(token, &Permission::Tool("search".into())).await?;
```

A grant the audit sink fails to record is refused with `SsoError::AuditFailed`, and a denial
stays `SsoError::AccessDenied`; see [When the Sink Fails](#when-the-sink-fails).

## Audit Logging

### FileAuditSink

```rust
use adk_auth::{AuthMiddleware, FileAuditSink};

let audit = FileAuditSink::new("/var/log/adk/audit.jsonl")?;
let middleware = AuthMiddleware::with_audit(ac, audit);
```

### When the Sink Fails

`ProtectedTool`, `ScopedTool`, the guards that build them, and `SsoAccessControl` refuse
access whose decision the audit sink fails to record. A tool call fails with
`auth.audit_failed` and the tool does not run; `SsoAccessControl::check_token` fails with
`SsoError::AuditFailed`. A denial is returned as a denial whether or not its record was
written.

| Mode | Effect when `AuditSink::log` fails |
|------|------------------------------------|
| `AuditFailureMode::Block` (default) | Access is refused |
| `AuditFailureMode::Warn` | A warning is logged and the access decision applies without a record |

```rust
use adk_auth::{AuditFailureMode, AuthMiddleware, ContextScopeResolver, ScopeGuard};

let middleware = AuthMiddleware::with_audit(ac, audit).with_audit_failure_mode(AuditFailureMode::Warn);
let guard = ScopeGuard::with_audit(ContextScopeResolver, audit_sink)
    .with_audit_failure_mode(AuditFailureMode::Warn);
```

```rust
use adk_auth::AuditFailureMode;
use adk_auth::sso::{GoogleProvider, SsoAccessControl};

let sso = SsoAccessControl::builder()
    .validator(GoogleProvider::new("client-id"))
    .access_control(ac)
    .audit_sink(audit)
    .audit_failure_mode(AuditFailureMode::Warn)
    .build()?;
```

### Tamper-Evident Chain

`FileAuditSink::with_chaining` writes each event with `prev_hash`, the SHA-256 of the
previous line; `with_hmac_chaining` uses HMAC-SHA256 under a key instead, so the chain
cannot be recomputed by someone who can write the file but does not hold the key. Opening
a chained file resumes from its last line, so the chain survives restarts.

```rust
use adk_auth::FileAuditSink;

let audit = FileAuditSink::with_hmac_chaining("/var/log/adk/audit.jsonl", key)?;
// ... log events ...
let events = audit.verify()?;

// Verify an archived file without opening it for writing.
let events = FileAuditSink::verify_file("/archive/audit.jsonl", Some(key.as_slice()))?;
```

`verify` fails with `AuthError::AuditError` naming the first line that was edited,
inserted, reordered, or removed. A change to the newest line and lines cut from the end
leave no broken link; record the newest line elsewhere to detect them.

The chained file has one writer and a symmetric key. For logs that several replicas write,
that a third party verifies with a public key, or that must detect truncation and rewrites,
use the signed audit log (`signed-audit` feature): RFC 8785 records, an RFC 9162 Merkle tree,
and Ed25519-signed checkpoints, kept in any `AuditLogStore` — in memory, SQLite, PostgreSQL,
Redis, MongoDB, Neo4j, or Firestore. `AuditLog` is itself an `AuditSink`. See
[Audit Log](audit-log.md).

| Sink | Integrity | Verifier needs | Writers |
|------|-----------|----------------|---------|
| `FileAuditSink::with_chaining` | SHA-256 chain | The file | One |
| `FileAuditSink::with_hmac_chaining` | HMAC-SHA256 chain | The file and the secret key | One |
| `signed_audit::AuditLog` | Chain, Merkle tree, signed checkpoints, optional anchors | The export and the public key | One per `log_id`, any number of replicas |

### Output Format (JSONL)

```json
{"timestamp":"2025-01-01T10:30:00Z","user":"bob","session_id":"sess-123","event_type":"tool_access","resource":"search","outcome":"allowed"}
{"timestamp":"2025-01-01T10:30:01Z","user":"bob","session_id":"sess-123","event_type":"tool_access","resource":"code_exec","outcome":"denied","tenant_id":"acme","agent_id":"spiffe://acme.example/agent/billing","on_behalf_of":{"kind":"human","id":"bob"}}
```

`ProtectedTool` and `ScopeGuard` record the calling agent on every decision:
`AuditEvent::with_acting_context` sets `agent_id`, `on_behalf_of`, and, when unset, `tenant_id`
from the tool context's acting context. `AuditFilter::agent_id` selects one agent's records, and
the PostgreSQL and OTLP sinks store both fields (`migrate()` adds the PostgreSQL columns to an
existing table). See [Agent Identity](agent-identity.md).

### Custom Audit Sink

```rust
use adk_auth::{AuditEvent, AuditSink, AuthError};
use async_trait::async_trait;

pub struct DatabaseAuditSink {
    pool: sqlx::PgPool,
}

#[async_trait]
impl AuditSink for DatabaseAuditSink {
    async fn log(&self, event: AuditEvent) -> Result<(), AuthError> {
        sqlx::query("INSERT INTO audit_log (user_id, resource, event) VALUES ($1, $2, $3)")
            .bind(&event.user)
            .bind(&event.resource)
            .bind(event.to_json().map_err(|e| AuthError::AuditError(e.to_string()))?)
            .execute(&self.pool)
            .await
            .map_err(|e| AuthError::AuditError(e.to_string()))?;
        Ok(())
    }
}
```

`log_batch`, `query`, `purge_before`, and `flush` have defaults; override them when the backend
supports them. `adk-auth` ships `PostgresAuditSink` (`postgres-audit` feature) and
`OtlpAuditSink` (`otlp-audit` feature).

## Examples

```bash
cargo check -p adk-auth
cargo check -p adk-auth --features sso
```

## Security Best Practices

| Practice | Description |
|----------|-------------|
| **Deny by default** | Only grant permissions explicitly needed |
| **Explicit denies** | Add deny rules for dangerous operations |
| **Audit everything** | Enable logging for compliance |
| **Validate server-side** | Always validate JWTs on the server |
| **Use HTTPS** | JWKS endpoints require secure connections |
| **Rotate keys** | JWKS cache auto-refreshes every hour |
| **Limit token lifetime** | Use short-lived access tokens |
| **Restrict Azure tenants** | For multi-tenant Azure apps, configure `with_allowed_tenants(...)` |
| **Verify email before identity mapping** | `user_id_from_email()` falls back to `sub` when the email is unverified |
| **Plan for revocation** | Token revocation is not built in; enforce it in a custom validator if you need immediate cut-off |
| **Cache expensive scope lookups** | If your `ScopeResolver` calls external systems, cache the result per request/session |

## Auth Bridge

Enable `auth-bridge` when you want `adk-auth` to provide a reusable JWT-based request extractor for `adk-server`:

```rust
use adk_auth::auth_bridge::JwtRequestContextExtractor;
use adk_auth::sso::{ClaimsMapper, GoogleProvider};

let extractor = JwtRequestContextExtractor::builder()
    .validator(GoogleProvider::new("client-id"))
    .mapper(ClaimsMapper::builder().user_id_from_email().build())
    .build()?;
```

The extractor validates the Bearer token, maps `user_id` with `ClaimsMapper`, and forwards JWT `scope` / `scp` claims into `RequestContext.scopes`.

### What the extractor protects

Configuring an extractor turns authentication on for every non-public route:

| Routes | Behaviour with an extractor configured |
|--------|----------------------------------------|
| `/api/sessions/*`, `/api/apps/*`, artifacts, debug | 401 without a valid token |
| `/api/ui/*` — bridge, notifications, resources | 401 without a valid token; the authenticated user replaces any user named in the request body |
| `/api/run*` | 401 without a valid token; the authenticated user overrides the supplied user |
| `POST /a2a`, `POST /a2a/stream` | 401 without a valid token |
| `/health`, `GET /.well-known/agent-card.json`, `GET /.well-known/agent.json` | Public — peers fetch the agent card before they hold a credential |

UI bridge state is keyed by `(app_name, user_id, session_id)` taken from the request
body, so the authenticated user is substituted for the body value rather than
trusted. A registered UI resource records the user that registered it; only that
user can read or replace it, and a read of someone else's resource answers 404 so
the URI's existence is not disclosed.

With **no** extractor configured there is no authenticated identity to bind, so
routes stay open and resources stay globally visible. Authentication is opt-in —
configure an extractor for any deployment that is not a single trusted user.

The A2A JSON-RPC routes carry the same layer as the session, artifact, and debug routes in
both `create_app_with_a2a` and `ServerBuilder::build`. `A2aServer::builder()` binds
`127.0.0.1:8080` by default; call `bind_addr` to expose it, and configure an extractor before
you do. The generated `a2a-server` scaffold reads `BIND_HOST` to opt into a wider bind. See
[Server](../deployment/server.md) for the loopback default, `allowed_origins`, and
`allowed_hosts`.

## Secret Providers

Tools reach runtime secrets through `ToolContext::get_secret` and
`InvocationContext::get_secret`. Behind those is `adk_auth::secrets::SecretProvider`,
with cloud implementations behind feature flags:

| Provider | Feature |
|----------|---------|
| AWS Secrets Manager | `aws-secrets` |
| Azure Key Vault | `azure-keyvault` |
| GCP Secret Manager | `gcp-secrets` |

Attach one to a run by wrapping it as a `SecretService`:

```rust
use adk_auth::secrets::{CachedSecretProvider, SecretProvider, SecretServiceAdapter};
use std::sync::Arc;
use std::time::Duration;

// Any SecretProvider — here wrapped in the cache
let cached = Arc::new(CachedSecretProvider::new(provider, Duration::from_secs(300)));
let service = Arc::new(SecretServiceAdapter::new(cached));
```

### Per-Tool Authorization

By default a tool holding a context can name any secret, and the provider sees only
that name — nothing distinguishes a weather tool asking for its own API key from the
same tool asking for a payment credential. `AuthorizingSecretService` decides per tool
before the provider is consulted:

```rust
use adk_auth::secrets::{AuthorizingSecretService, SecretGrant};
use std::sync::Arc;

let service = Arc::new(
    AuthorizingSecretService::new(inner)
        .grant("weather_lookup", SecretGrant::none().name("weather-api-key"))
        .grant("charge_card", SecretGrant::none().prefix("billing/"))
        .with_audit_sink(audit_sink),
);
```

| Rule | Behaviour |
|------|-----------|
| Tool has a grant covering the name | Allowed |
| Tool has a grant that does not cover the name | Denied; the provider is never called |
| Tool has no grant | Denied |
| Request carries no tool identity | Denied unless `grant_untooled` opens it |

Everything is denied until granted, and a denial returns an `Unauthorized` error. A
denied name is never looked up, so it does not appear in provider-side access logs as
an attempted read.

The identity is not something a tool asserts. `LlmAgent` stamps the dispatched tool's
name onto the request, alongside the app, user, session, and invocation, so a tool
cannot present another tool's identity. A tool can add only a *purpose*:

```rust
// inside a tool
let key = ctx.get_secret_for_purpose("weather-api-key", "call the forecast endpoint").await?;
```

> **Note:** an agent invoked as a tool crosses a `ToolContext`, which carries no
> identity of its own, so accesses made inside that agent present the outer agent's
> identity rather than the inner tool's. Grant accordingly.

### Auditing Access

`SecretAuditSink` receives one `SecretAccessDecision` per decision, carrying the
outcome, the secret name, the tool, the user, the invocation, and the reason — and
never a secret value. Allows are also logged at `info` and denials at `warn`.

### Caching

`CachedSecretProvider` serves a value for its TTL, then refetches. It is bounded and
revocable:

| Control | Behaviour |
|---------|-----------|
| `with_max_entries(n)` | At most `n` names cached; the least recently used is dropped when full. Defaults to 128; `0` disables caching |
| `invalidate(name)` | Drops one secret immediately — use this when a secret is rotated so the old value is not served for the rest of its TTL |
| `invalidate_all()` | Drops everything |
| `purge_expired()` | Drops expired entries without waiting for them to be read again |

A bound matters when secret names are derived from input: without one, the cache can
grow for the lifetime of the process.

### What the cache does and does not guarantee

A TTL controls what the cache **returns**, not how long a value stays in process
memory. Entries are zeroized when they expire, are evicted, or are invalidated, which
shortens residency to roughly the TTL. That is a reduction, not erasure — a `String`
may already have been reallocated, copied by the allocator, swapped to disk, or
captured in a core dump. Debug output for the cache is redacted so a diagnostic print
cannot leak a value.

> **Important:** a bare `SecretProvider` applies no policy of its own — any tool
> holding a context can request any name the backing credentials can read. Wrap it in
> [`AuthorizingSecretService`](#per-tool-authorization) to get a per-tool boundary, and
> still scope the cloud credentials themselves: one IAM identity per deployment with
> access to only the secrets that deployment needs.

## Error Handling

```rust
use adk_auth::{AccessDenied, Permission};
use adk_auth::sso::{TokenError, TokenValidator};

// RBAC errors
match ac.check("user", &Permission::tool("admin")) {
    Ok(()) => { /* access granted */ }
    Err(AccessDenied { user, permission }) => {
        tracing::warn!(user = %user, permission = %permission, "access denied");
    }
}

// SSO errors
match provider.validate(token).await {
    Ok(claims) => { /* token valid */ }
    Err(TokenError::Expired) => { /* token expired */ }
    Err(TokenError::InvalidSignature) => { /* signature invalid */ }
    Err(TokenError::MissingClaim(claim)) => { /* no `exp`, `iss`, or `aud` claim */ }
    Err(TokenError::ValidationError(reason)) => { /* wrong issuer or audience, among others */ }
    Err(e) => { /* other error */ }
}
```

`OidcProvider::from_discovery` fails with `TokenError::InvalidIssuer` when the discovery
document names another issuer. `AuthError` and `TokenError` convert into `AdkError` with
codes such as `auth.access_denied` (`Forbidden`) and `auth.token_expired` (`Unauthorized`).

---

**Previous**: [← Evaluation](../evaluation/evaluation.md) | **Next**: [Tool Authorization →](tool-authorization.md)
