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
let role = Role::new("limited")
    .allow(Permission::AllTools)      // Allow everything...
    .deny(Permission::Tool("admin")); // ...except admin

// Result: Can access any tool EXCEPT "admin"
```

### 2. Multi-Role Union

Users with multiple roles get the **union** of permissions, but deny rules from any role still apply:

```rust
let ac = AccessControl::builder()
    .role(reader)    // allow: search
    .role(writer)    // allow: write
    .assign("alice", "reader")
    .assign("alice", "writer")
    .build()?;

// Alice can access both "search" AND "write"
```

### 3. Explicit Over Implicit

Permissions are explicit - no access is granted by default:

```rust
let role = Role::new("empty");
// This role grants NO permissions

ac.check("user", &Permission::Tool("anything")); // → Denied
```

### 4. Separation of Authentication and Authorization

- **Authentication** (SSO): Verifies WHO the user is (via JWT)
- **Authorization** (RBAC): Determines WHAT they can access

```rust
// Authentication: validate JWT, extract claims
let claims = provider.validate(token).await?;

// Authorization: check specific permission
ac.check(&claims.sub, &Permission::Tool("search"))?;

// Combined: SsoAccessControl does both
sso.check_token(token, &permission).await?;
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

### Role

```rust
let analyst = Role::new("analyst")
    .allow(Permission::Tool("search".into()))
    .allow(Permission::Tool("summarize".into()))
    .deny(Permission::Tool("code_exec".into()));
```

### AccessControl

```rust
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

Wraps a tool with automatic permission checking:

```rust
use adk_auth::ToolExt;

let protected = my_tool.with_access_control(Arc::new(ac));

// When executed, checks permission before running
protected.execute(ctx, args).await?;
```

### AuthMiddleware

Batch-protect multiple tools:

```rust
let middleware = AuthMiddleware::new(ac);
let protected_tools = middleware.protect_all(tools);
```

### ScopeGuard

Use scopes for request-level authorization that comes from JWT claims or session state:

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
| Generic | `OidcProvider::from_discovery(issuer, client)` | Any OIDC provider |

`AzureADProvider::multi_tenant()` accepts any tenant for the configured audience unless you explicitly restrict it with `with_allowed_tenants(...)`.

### TokenClaims

Claims extracted from validated JWTs:

```rust
pub struct TokenClaims {
    pub sub: String,              // Subject (user ID)
    pub email: Option<String>,    // Email
    pub name: Option<String>,     // Display name
    pub groups: Vec<String>,      // IdP groups
    pub roles: Vec<String>,       // IdP roles
    pub hd: Option<String>,       // Google hosted domain
    pub tid: Option<String>,      // Azure tenant ID
    // ... more standard OIDC claims
}
```

### ClaimsMapper

Maps IdP groups to adk-auth roles:

```rust
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
let sso = SsoAccessControl::builder()
    .validator(GoogleProvider::new("client-id"))
    .mapper(mapper)
    .access_control(ac)
    .audit_sink(audit)
    .build()?;

// Validate token + check permission + audit log
let claims = sso.check_token(token, &Permission::Tool("search".into())).await?;
```

## Audit Logging

### FileAuditSink

```rust
let audit = FileAuditSink::new("/var/log/adk/audit.jsonl")?;
let middleware = AuthMiddleware::with_audit(ac, audit);
```

### Output Format (JSONL)

```json
{"timestamp":"2025-01-01T10:30:00Z","user":"bob","session_id":"sess-123","event_type":"tool_access","resource":"search","outcome":"allowed"}
{"timestamp":"2025-01-01T10:30:01Z","user":"bob","session_id":"sess-123","event_type":"tool_access","resource":"code_exec","outcome":"denied"}
```

### Custom Audit Sink

```rust
use adk_auth::{AuditSink, AuditEvent, AuthError};
use async_trait::async_trait;

pub struct DatabaseAuditSink { /* ... */ }

#[async_trait]
impl AuditSink for DatabaseAuditSink {
    async fn log(&self, event: AuditEvent) -> Result<(), AuthError> {
        // Insert into database
        sqlx::query("INSERT INTO audit_log ...")
            .bind(event.user)
            .bind(event.resource)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
```

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
| **Verify email before identity mapping** | `user_id_from_email()` now falls back to `sub` when email is unverified |
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
use adk_auth::secrets::authorizing::{AuthorizingSecretService, SecretGrant};
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
> **Important:** the provider interface takes only a secret *name*. There is no
> per-tool grant, namespace, or access audit at the ADK layer, so any tool holding a
> context can request any name the backing credentials can read. Scope the cloud
> credentials themselves — one IAM identity per deployment with access to only the
> secrets that deployment needs — and treat provider-side audit logs as the record of
> access.
### What the extractor protects

Configuring an extractor turns authentication on for every non-public route:

| Routes | Behaviour with an extractor configured |
|--------|----------------------------------------|
| `/api/sessions/*`, `/api/apps/*`, artifacts, debug | 401 without a valid token |
| `/api/ui/*` — bridge, notifications, resources | 401 without a valid token; the authenticated user replaces any user named in the request body |
| `/api/run*` | 401 without a valid token; the authenticated user overrides the supplied user |
| `/health` | Public |

UI bridge state is keyed by `(app_name, user_id, session_id)` taken from the request
body, so the authenticated user is substituted for the body value rather than
trusted. A registered UI resource records the user that registered it; only that
user can read or replace it, and a read of someone else's resource answers 404 so
the URI's existence is not disclosed.

With **no** extractor configured there is no authenticated identity to bind, so
routes stay open and resources stay globally visible. Authentication is opt-in —
configure an extractor for any deployment that is not a single trusted user.

## Error Handling

```rust
use adk_auth::{AccessDenied, AuthError};
use adk_auth::sso::TokenError;

// RBAC errors
match ac.check("user", &Permission::Tool("admin".into())) {
    Ok(()) => { /* access granted */ }
    Err(AccessDenied { user, permission }) => {
        eprintln!("Denied: {} cannot access {}", user, permission);
    }
}

// SSO errors
match provider.validate(token).await {
    Ok(claims) => { /* token valid */ }
    Err(TokenError::Expired) => { /* token expired */ }
    Err(TokenError::InvalidSignature) => { /* signature invalid */ }
    Err(TokenError::InvalidIssuer { expected, actual }) => { /* wrong issuer */ }
    Err(e) => { /* other error */ }
}
```


---

**Previous**: [← Evaluation](../evaluation/evaluation.md) | **Next**: [Tool Authorization →](tool-authorization.md)

## A2A endpoints

| Route | Authentication |
|-------|----------------|
| `GET /.well-known/agent.json` | **public** — peers fetch the card before they hold a credential |
| `POST /a2a` | required, when a `RequestContextExtractor` is configured |
| `POST /a2a/stream` | required, when a `RequestContextExtractor` is configured |

The JSON-RPC routes execute agent and tool work, so they carry the same layer as the session,
artifact, and debug routers. With no extractor configured there is no credential to demand and
the routes stay open, so adding the gate does not break an existing deployment.

> **Important:** these routes were previously merged at the router root, outside the layer
> applied to `/api`. A deployment that authenticated every other mutation surface still let any
> client that could reach the port drive the agent and incur its cost. This applied to both
> `create_app_with_a2a` and `ServerBuilder::build`.

`A2aServer::builder()` binds `127.0.0.1:8080` by default. Call `bind_addr` to expose it, and
configure an extractor before you do. The generated `a2a-server` scaffold follows the same rule
and reads `BIND_HOST` to opt into a wider bind.
