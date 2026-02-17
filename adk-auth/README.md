# adk-auth

Access control and authentication for Rust Agent Development Kit (ADK-Rust).

[![Crates.io](https://img.shields.io/crates/v/adk-auth.svg)](https://crates.io/crates/adk-auth)
[![Documentation](https://docs.rs/adk-auth/badge.svg)](https://docs.rs/adk-auth)
[![License](https://img.shields.io/crates/l/adk-auth.svg)](LICENSE)

## Overview

`adk-auth` provides enterprise-grade access control for AI agents:

- **Declarative Scope-Based Security** - Tools declare required scopes, framework enforces automatically
- **Role-Based Access** - Define roles with tool/agent permissions (allow/deny, deny precedence)
- **Audit Logging** - Log all access attempts to JSONL files
- **SSO/OAuth** - JWT validation with Google, Azure AD, Okta, Auth0 providers

## Features

| Feature | Description |
|---------|-------------|
| `default` | Core RBAC + scope-based security + audit logging |
| `sso` | JWT/OIDC providers (Google, Azure AD, Okta, Auth0) |

## Declarative Scope-Based Security

Tools declare what scopes they need. The framework enforces before execution — no imperative checks in your handlers:

```rust
use adk_tool::FunctionTool;
use adk_auth::{ScopeGuard, ContextScopeResolver, StaticScopeResolver};

// Tool declares its required scopes
let transfer = FunctionTool::new("transfer", "Transfer funds", handler)
    .with_scopes(&["finance:write", "verified"]);

// ScopeGuard enforces automatically
let guard = ScopeGuard::new(ContextScopeResolver);
let protected = guard.protect(transfer);

// Or wrap all tools at once
let protected_tools = guard.protect_all(tools);
```

With audit logging:

```rust
let guard = ScopeGuard::with_audit(ContextScopeResolver, audit_sink);
let protected = guard.protect(transfer);
// All scope checks (allowed + denied) are logged
```

Pluggable resolvers:

| Resolver | Source |
|----------|--------|
| `ContextScopeResolver` | Delegates to `ToolContext::user_scopes()` (JWT claims, session state) |
| `StaticScopeResolver` | Fixed scopes — useful for testing |
| Custom `impl ScopeResolver` | Any async source (database, external IdP, etc.) |

## Role-Based Access Control

```rust
use adk_auth::{Permission, Role, AccessControl, AuthMiddleware};

// Define roles
let admin = Role::new("admin").allow(Permission::AllTools);
let user = Role::new("user")
    .allow(Permission::Tool("search".into()))
    .deny(Permission::Tool("code_exec".into()));

// Build access control
let ac = AccessControl::builder()
    .role(admin)
    .role(user)
    .assign("alice@example.com", "admin")
    .assign("bob@example.com", "user")
    .build()?;

// Protect tools
let middleware = AuthMiddleware::new(ac);
let protected_tools = middleware.protect_all(tools);
```

## SSO Integration

Enable with `features = ["sso"]`:

```rust
use adk_auth::sso::{GoogleProvider, ClaimsMapper, SsoAccessControl};

// Create provider
let provider = GoogleProvider::new("your-client-id");

// Map IdP groups to roles
let mapper = ClaimsMapper::builder()
    .map_group("AdminGroup", "admin")
    .default_role("viewer")
    .user_id_from_email()
    .build();

// Combined SSO + RBAC
let sso = SsoAccessControl::builder()
    .validator(provider)
    .mapper(mapper)
    .access_control(ac)
    .build()?;

// Validate token and check permission
let claims = sso.check_token(token, &Permission::Tool("search".into())).await?;
println!("User: {}", claims.email.unwrap());
```

## Providers

| Provider | Usage |
|----------|-------|
| **Google** | `GoogleProvider::new(client_id)` |
| **Azure AD** | `AzureADProvider::new(tenant_id, client_id)` |
| **Okta** | `OktaProvider::new(domain, client_id)` |
| **Auth0** | `Auth0Provider::new(domain, audience)` |
| **Generic OIDC** | `OidcProvider::from_discovery(issuer, client_id).await` |

## Audit Logging

```rust
use adk_auth::FileAuditSink;

let audit = FileAuditSink::new("/var/log/adk/audit.jsonl")?;
let middleware = AuthMiddleware::with_audit(ac, audit);
```

Output:
```json
{"timestamp":"2025-01-01T10:30:00Z","user":"bob","resource":"search","outcome":"allowed"}
```

## Examples

```bash
cargo run --example auth_basic          # RBAC basics
cargo run --example auth_audit          # Audit logging
cargo run --example auth_sso --features sso     # SSO integration
cargo run --example auth_jwt --features sso     # JWT validation
cargo run --example auth_oidc --features sso    # OIDC discovery
cargo run --example auth_google --features sso  # Google Identity
```

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
