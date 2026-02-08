# Access Control

Enterprise-grade access control for AI agents using `adk-auth`.

## Overview

`adk-auth` provides role-based access control (RBAC) with audit logging and SSO support for ADK agents. It enables secure, fine-grained control over which users can access which tools.

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
adk-auth = "0.3.0"

# For SSO/OAuth support
adk-auth = { version = "0.3.0", features = ["sso"] }
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

## SSO Integration

### Supported Providers

| Provider | Constructor | Issuer |
|----------|-------------|--------|
| Google | `GoogleProvider::new(client_id)` | accounts.google.com |
| Azure AD | `AzureADProvider::new(tenant, client)` | login.microsoftonline.com |
| Okta | `OktaProvider::new(domain, client)` | {domain}/oauth2/default |
| Auth0 | `Auth0Provider::new(domain, audience)` | {domain}/ |
| Generic | `OidcProvider::from_discovery(issuer, client)` | Any OIDC provider |

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
# Core RBAC
cargo run --example auth_basic          # Role-based access control
cargo run --example auth_audit          # Audit logging

# SSO (requires --features sso)
cargo run --example auth_sso --features sso     # Complete SSO flow
cargo run --example auth_jwt --features sso     # JWT validation
cargo run --example auth_oidc --features sso    # OIDC discovery
cargo run --example auth_google --features sso  # Google Identity
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

**Previous**: [← Evaluation](../evaluation/evaluation.md) | **Next**: [Development Guidelines →](../development/development-guidelines.md)
