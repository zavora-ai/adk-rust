# adk-auth

Access control and authentication for Rust Agent Development Kit (ADK-Rust).

[![Crates.io](https://img.shields.io/crates/v/adk-auth.svg)](https://crates.io/crates/adk-auth)
[![Documentation](https://docs.rs/adk-auth/badge.svg)](https://docs.rs/adk-auth)
[![License](https://img.shields.io/crates/l/adk-auth.svg)](LICENSE)

## Overview

`adk-auth` provides enterprise-grade access control for AI agents:

- Declarative scope-based security — tools declare required scopes, framework enforces automatically
- Role-based access control — define roles with allow/deny permissions, deny takes precedence
- Audit logging — log all access attempts to JSONL files
- SSO/OAuth — JWT validation with Google, Azure AD, Okta, Auth0, and generic OIDC providers
- Auth bridge — flow authenticated identity from HTTP requests into agent execution via `adk-server`

## Installation

```toml
[dependencies]
adk-auth = "0.5.0"

# With SSO/JWT validation
adk-auth = { version = "0.5.0", features = ["sso"] }

# With auth bridge for adk-server identity flow (implies sso)
adk-auth = { version = "0.5.0", features = ["auth-bridge"] }
```

## Features

Core RBAC, scope-based security, and audit logging are always available with no feature flags.

| Feature | Description |
|---------|-------------|
| `sso` | JWT/OIDC providers (Google, Azure AD, Okta, Auth0, generic OIDC) |
| `auth-bridge` | `JwtRequestContextExtractor` for `adk-server` identity flow (implies `sso`) |

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

You can also use the extension trait for inline wrapping:

```rust
use adk_auth::ScopeToolExt;

let protected = my_tool.with_scope_guard(ContextScopeResolver);
```

Pluggable resolvers:

| Resolver | Source |
|----------|--------|
| `ContextScopeResolver` | Delegates to `ToolContext::user_scopes()` (JWT claims, session state) |
| `StaticScopeResolver` | Fixed scopes — useful for testing |
| Custom `impl ScopeResolver` | Any async source (database, external IdP, etc.) |

For resolvers that call external services, cache the resolved scopes at the request or session layer to avoid repeated lookups during multi-tool runs.

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

Deny always takes precedence over allow, regardless of role assignment order. If a user has both an `editor` role (allow all tools) and a `restricted` role (deny `code_exec`), `code_exec` is denied.

You can also use the extension trait:

```rust
use adk_auth::ToolExt;

let protected = my_tool.with_access_control(Arc::new(ac));
```

## Combining RBAC + Scopes

Use RBAC for coarse tool/agent entitlement and scopes for request-level constraints:

```rust
use std::sync::Arc;
use adk_auth::{AuthMiddleware, ContextScopeResolver, ScopeGuard};

let rbac = AuthMiddleware::new(ac);
let scoped = ScopeGuard::new(ContextScopeResolver);

let protected = scoped.protect(rbac.protect(transfer_tool));
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
println!("User: {}", claims.user_id());
```

### User ID Claim Selection

The `ClaimsMapper` builder controls which JWT claim becomes the `user_id`:

| Method | Claim | Fallback |
|--------|-------|----------|
| `user_id_from_sub()` (default) | `sub` | — |
| `user_id_from_email()` | `email` (only when `email_verified == true`) | `sub` |
| `user_id_from_preferred_username()` | `preferred_username` | `sub` |
| `user_id_from_claim("custom")` | Any custom claim | `sub` |

## Providers

| Provider | Usage |
|----------|-------|
| Google | `GoogleProvider::new(client_id)` |
| Azure AD | `AzureADProvider::new(tenant_id, client_id)` or `::multi_tenant(client_id).with_allowed_tenants(["tenant-id"])` |
| Okta | `OktaProvider::new(domain, client_id)` or `::with_auth_server(domain, server_id, client_id)` |
| Auth0 | `Auth0Provider::new(domain, audience)` |
| Generic OIDC | `OidcProvider::from_discovery(issuer, client_id).await` or `::new(issuer, client_id, jwks_uri)` |
| Custom JWT | `JwtValidator::builder().issuer(iss).jwks_uri(uri).audience(aud).build()?` |

`AzureADProvider::multi_tenant()` accepts tokens from any tenant targeting the configured audience unless you restrict it with `with_allowed_tenants(...)`.

`OidcProvider::from_discovery()` rejects discovery documents whose `issuer` does not match the requested issuer URL.

All providers implement the `TokenValidator` trait — you can implement it for any custom identity provider.

## Auth Bridge

Enable with `features = ["auth-bridge"]` to validate Bearer tokens directly into `adk-server` request contexts:

```rust
use adk_auth::auth_bridge::JwtRequestContextExtractor;
use adk_auth::sso::{ClaimsMapper, GoogleProvider};

let extractor = JwtRequestContextExtractor::builder()
    .validator(GoogleProvider::new("your-client-id"))
    .mapper(ClaimsMapper::builder().user_id_from_email().build())
    .build()?;
```

The extractor maps:

- `user_id` from the configured `ClaimsMapper`
- `scopes` from JWT `scope` (space-delimited string) and `scp` (array) claims, deduplicated
- `metadata` including issuer, subject, email, tenant ID, and hosted domain when present

## Audit Logging

```rust
use adk_auth::FileAuditSink;

let audit = FileAuditSink::new("/var/log/adk/audit.jsonl")?;
let middleware = AuthMiddleware::with_audit(ac, audit);
```

Output:
```json
{"timestamp":"2025-01-01T10:30:00Z","user":"bob","event_type":"tool_access","resource":"search","outcome":"allowed"}
```

Implement the `AuditSink` trait for custom destinations (database, external service, etc.).

## Error Types

| Type | When |
|------|------|
| `AccessDenied` | RBAC check fails (user lacks permission) |
| `AuthError` | Role not found, audit sink failure |
| `ScopeDenied` | Scope check fails (missing required scopes) |
| `TokenError` | JWT validation failure (expired, bad signature, missing claims) |
| `SsoError` | Token validation or access denied in SSO flow |

## Examples

Examples live in the [adk-playground](https://github.com/zavora-ai/adk-playground) repo:

```bash
git clone https://github.com/zavora-ai/adk-playground.git
cd adk-playground

cargo run --example auth_basic                  # RBAC basics
cargo run --example auth_audit                  # Audit logging
cargo run --example auth_bridge                 # Auth bridge with server
cargo run --example auth_sso --features sso     # SSO integration
cargo run --example auth_jwt --features sso     # JWT validation
cargo run --example auth_oidc --features sso    # OIDC discovery
cargo run --example auth_google --features sso  # Google Identity
```

## Security Notes

- Prefer short-lived access tokens and rotate signing keys regularly.
- The JWKS cache refreshes hourly by default; lower the refresh interval with `JwksCache::with_refresh_interval()` if your IdP rotates keys aggressively.
- `OidcProvider::from_discovery()` rejects discovery documents whose `issuer` does not match the requested issuer URL.
- Token revocation and blacklist checks are not built in. If you need immediate revocation, enforce it in a custom `TokenValidator` or request extractor.
- `JwtValidator` rejects symmetric algorithms (HS256/HS384/HS512) and EdDSA — only RSA and EC algorithms are supported with JWKS-based validation.

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
