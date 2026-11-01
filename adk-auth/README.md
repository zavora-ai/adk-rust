# adk-auth

Access control and authentication for Rust Agent Development Kit (ADK-Rust).

[![Crates.io](https://img.shields.io/crates/v/adk-auth.svg)](https://crates.io/crates/adk-auth)
[![Documentation](https://docs.rs/adk-auth/badge.svg)](https://docs.rs/adk-auth)
[![License](https://img.shields.io/crates/l/adk-auth.svg)](../LICENSE)

## Overview

`adk-auth` provides access control for AI agents:

- Declarative scope-based security — tools declare required scopes, framework enforces automatically
- Role-based access control — define roles with allow/deny permissions, deny takes precedence
- Audit logging — every access decision to a JSONL file, PostgreSQL, OTLP, or a signed Merkle-tree log
- Credential brokers — audience-bound tokens for an agent's outbound calls
- Secret providers — AWS Secrets Manager, Azure Key Vault, and GCP Secret Manager
- SSO/OAuth — JWT validation with Google, Azure AD, Okta, Auth0, and generic OIDC providers
- Auth bridge — flow authenticated identity from HTTP requests into agent execution via `adk-server`
- Web Bot Auth — verify and sign RFC 9421 request signatures so automated clients have a verified identity

## Installation

```toml
[dependencies]
adk-auth = "3.0.0"

# With SSO/JWT validation
adk-auth = { version = "3.0.0", features = ["sso"] }

# With auth bridge for adk-server identity flow (implies sso)
adk-auth = { version = "3.0.0", features = ["auth-bridge"] }

# With the signed, Merkle-hashed audit log
adk-auth = { version = "3.0.0", features = ["signed-audit"] }

# With Web Bot Auth request signatures
adk-auth = { version = "3.0.0", features = ["web-bot-auth"] }
```

## Features

Core RBAC, scope-based security, and audit logging are always available with no feature flags.

| Feature | Description |
|---------|-------------|
| `sso` | JWT/OIDC providers (Google, Azure AD, Okta, Auth0, generic OIDC) |
| `auth-bridge` | `JwtRequestContextExtractor` for `adk-server` identity flow (implies `sso`) |
| `token-exchange` | `TokenExchangeBroker`: RFC 8693 token exchange of the agent's credential, or of the subject's token with the agent as actor (`on_behalf_of`), for resource-scoped tokens |
| `signed-audit` | `signed_audit::AuditLog`: the signed, Merkle-hashed audit log, and `signed_audit::verify` |
| `web-bot-auth` | `web_bot_auth`: RFC 9421 Ed25519 request signatures, `HttpSignatureVerifier`, `HttpSignatureSigner`, key directories |
| `postgres-audit` | `PostgresAuditSink` |
| `otlp-audit` | `OtlpAuditSink` |
| `aws-secrets` | `secrets::AwsSecretProvider` (AWS Secrets Manager) |
| `azure-keyvault` | `secrets::AzureSecretProvider` (Azure Key Vault) |
| `gcp-secrets` | `secrets::GcpSecretProvider` (GCP Secret Manager) |

## Declarative Scope-Based Security

Tools declare what scopes they need. The framework enforces before execution — no imperative checks in your handlers:

```rust
use std::sync::Arc;

use adk_auth::{ContextScopeResolver, InMemoryAuditSink, ScopeGuard, ScopeToolExt};
use adk_core::{Result, Tool, ToolContext};
use adk_tool::FunctionTool;
use serde_json::{Value, json};

async fn transfer_funds(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
    Ok(json!({ "transferred": args["amount"] }))
}

fn protect(tools: Vec<Arc<dyn Tool>>) {
    // The tool declares its required scopes
    let transfer = FunctionTool::new("transfer", "Transfer funds", transfer_funds)
        .with_scopes(&["finance:write", "verified"]);

    // ScopeGuard enforces them before execute()
    let guard = ScopeGuard::new(ContextScopeResolver);
    let _protected = guard.protect(transfer);

    // Or wrap every tool at once
    let _protected_tools = guard.protect_all(tools);

    // With an audit sink, every check (allowed and denied) is recorded
    let audited = ScopeGuard::with_audit(ContextScopeResolver, InMemoryAuditSink::new());
    let _audited_tool = audited
        .protect(FunctionTool::new("transfer", "Transfer funds", transfer_funds));

    // Or wrap inline with the extension trait
    let _inline = FunctionTool::new("transfer", "Transfer funds", transfer_funds)
        .with_scope_guard(ContextScopeResolver);
}
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
use std::sync::Arc;

use adk_auth::{AccessControl, AuthMiddleware, Permission, Role};
use adk_core::Tool;

fn rbac(tools: Vec<Arc<dyn Tool>>) -> Result<Vec<Arc<dyn Tool>>, adk_auth::AuthError> {
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
    Ok(middleware.protect_all(tools))
}
```

Deny always takes precedence over allow, regardless of role assignment order. If a user has both an `editor` role (allow all tools) and a `restricted` role (deny `code_exec`), `code_exec` is denied.

You can also use the extension trait:

```rust
use std::sync::Arc;

use adk_auth::{AccessControl, ToolExt};
use adk_core::Tool;

fn protect_one(tool: impl Tool + 'static, ac: AccessControl) -> impl Tool {
    tool.with_access_control(Arc::new(ac))
}
```

## Combining RBAC + Scopes

Use RBAC for coarse tool/agent entitlement and scopes for request-level constraints:

```rust
use adk_auth::{AccessControl, AuthMiddleware, ContextScopeResolver, ScopeGuard};
use adk_core::Tool;

fn layered(ac: AccessControl, transfer_tool: impl Tool + 'static) -> impl Tool {
    let rbac = AuthMiddleware::new(ac);
    let scoped = ScopeGuard::new(ContextScopeResolver);
    scoped.protect(rbac.protect(transfer_tool))
}
```

## SSO Integration

Enable with `features = ["sso"]`:

```rust
use adk_auth::sso::{ClaimsMapper, GoogleProvider, SsoAccessControl};
use adk_auth::{AccessControl, Permission};

async fn check(ac: AccessControl, token: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Create a provider
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

    // Validate the token and check a permission
    let claims = sso.check_token(token, &Permission::Tool("search".into())).await?;
    println!("User: {}", claims.user_id());
    Ok(())
}
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
| Custom JWT | `JwtValidator::builder().issuer(iss).audience(aud).jwks_uri(uri).build()?` |

Every validator requires the `exp`, `iss`, and `aud` claims: a token without one fails with `TokenError::MissingClaim`. `JwtValidatorBuilder::build` fails without an audience; for an issuer whose tokens carry no `aud` claim, `without_audience()` opts out, and the validator then refuses any token that carries an audience and still requires `iss`.

`AzureADProvider::multi_tenant()` accepts tokens from any tenant targeting the configured audience unless you restrict it with `with_allowed_tenants(...)`. It requires the `iss` claim but does not check its value.

`OidcProvider::from_discovery()` rejects discovery documents whose `issuer` does not match the requested issuer URL.

All providers implement the `TokenValidator` trait — you can implement it for any custom identity provider.

## Auth Bridge

Enable with `features = ["auth-bridge"]` to validate Bearer tokens directly into `adk-server` request contexts:

```rust
use adk_auth::auth_bridge::JwtRequestContextExtractor;
use adk_auth::sso::{ClaimsMapper, GoogleProvider};

fn extractor() -> Result<JwtRequestContextExtractor, Box<dyn std::error::Error>> {
    Ok(JwtRequestContextExtractor::builder()
        .validator(GoogleProvider::new("your-client-id"))
        .mapper(ClaimsMapper::builder().user_id_from_email().build())
        .build()?)
}
```

Pass it to `ServerConfig::with_request_context(Arc::new(extractor))`.

The extractor maps:

- `user_id` from the configured `ClaimsMapper`
- `scopes` from JWT `scope` (space-delimited string) and `scp` (array) claims, deduplicated
- `metadata` including issuer, subject, email, tenant ID, and hosted domain when present

## Audit Logging

```rust
use adk_auth::{AccessControl, AuthMiddleware, FileAuditSink};

fn audited(ac: AccessControl) -> std::io::Result<AuthMiddleware> {
    let audit = FileAuditSink::new("/var/log/adk/audit.jsonl")?;
    Ok(AuthMiddleware::with_audit(ac, audit))
}
```

Each line is one `AuditEvent`; optional fields such as `session_id`, `agent_id`, and `on_behalf_of` appear when set:

```json
{"timestamp":"2026-10-01T10:30:00Z","user":"bob","event_type":"tool_access","resource":"search","outcome":"allowed","agent_id":"urn:adk:agent:ops/assistant"}
```

Implement the `AuditSink` trait for custom destinations (database, external service, etc.).

A protected or scoped tool refuses a call whose decision the sink fails to record
(`AuditFailureMode::Block`, the default); `with_audit_failure_mode(AuditFailureMode::Warn)`
logs the failure and continues instead. `SsoAccessControl` refuses such a grant with
`SsoError::AuditFailed`, and its builder takes the same mode through `audit_failure_mode`.
`FileAuditSink::with_chaining` and `with_hmac_chaining` write a hash chain that resumes
after a restart, and `FileAuditSink::verify` reports the first line whose `prev_hash` does
not match the line before it: an edit, insertion, or removal is caught at the next line,
while a change to the newest line and lines cut from the end leave no broken link.

Protected and scoped tools record the acting agent on every decision: `agent_id`,
`on_behalf_of`, and the principal's tenant (`AuditEvent::with_acting_context`).
`AuditFilter::agent_id` selects one agent's records.

### Signed audit log (`signed-audit`)

`signed_audit::AuditLog` writes RFC 8785 records into an RFC 9162 Merkle tree per replica,
signs C2SP checkpoints with Ed25519 (Go `sumdb/note` compatible), rotates keys with a
dual-signed checkpoint, and publishes checkpoints to optional anchors. It is an `AuditSink`
and an `AuditRecorder`, so `Runner::builder().audit_log(log)` records every governed tool
decision, approval, kill-switch change, payment, and unknown outcome of a run in it.
Records live in an `AuditLogStore` (in memory, or `adk-session` SQLite, PostgreSQL, Redis,
MongoDB, Neo4j, or Firestore), and
`signed_audit::verify` or `adk-rust audit verify` detects flipped bytes, deletions,
reorders, forks, and the truncation of records that a supplied checkpoint covers. An export
cut together with its later checkpoints, or an empty one, verifies unless the anchored
checkpoints are passed as well (`--checkpoints`). See the
[Audit Log guide](../docs/official_docs/security/audit-log.md).

### Pseudonymous events

`PseudonymousAuditSink` wraps any `AuditSink` and rewrites each event before it is chained:
`user` and `on_behalf_of` become `adk_core::subject::SubjectRegistry` pseudonyms (visitors by
default, every subject after `for_every_subject()`), and `ip_address` is dropped. Erasing a
subject deletes its pseudonym, so the stored events stay chained and no longer name it.

## Web Bot Auth (`web-bot-auth`)

`web_bot_auth` implements the IETF Web Bot Auth draft
(`draft-ietf-webbotauth-httpsig-protocol-00`) over HTTP Message Signatures (RFC 9421) with
Ed25519 keys. An automated client signs each request and names, in `Signature-Agent`, the
origin whose `/.well-known/http-message-signatures-directory` publishes its key; a site
verifies the signature against that directory instead of trusting a `User-Agent` claim.

| Type | Role |
|------|------|
| `HttpSignatureVerifier` | Verifies a request; returns `VerifiedRequester` with `id = "wba:{directory origin}#{key thumbprint}"`, or `None` for an unsigned request |
| `HttpSignatureSigner` | Signs an `http::Request` or `reqwest::Request` with a site's key and `Signature-Agent` |
| `HttpKeyDirectorySource` | Fetches directories over HTTPS: public addresses only, no redirects, 5 s timeout, 64 KiB and 64 keys at most |
| `InMemoryKeyDirectorySource` | Directories from memory, for tests and out-of-band keys |
| `KeyDirectory` | Parses a directory, and builds the one a site publishes, with its possession proof |
| `Ed25519SigningKey` | Ed25519 key on `aws-lc-rs`; the seed is zeroized and never printed |

```rust
use std::sync::Arc;

use adk_auth::web_bot_auth::{HttpKeyDirectorySource, HttpSignatureVerifier, VerifiedRequester};

async fn requester(
    verifier: &HttpSignatureVerifier,
    request: &http::Request<()>,
) -> Option<VerifiedRequester> {
    // Unsigned requests and failed signatures both earn no identity.
    verifier.verify_request(request).await.ok().flatten()
}

fn verifier() -> HttpSignatureVerifier {
    let directories = Arc::new(HttpKeyDirectorySource::new().expect("HTTP client"));
    HttpSignatureVerifier::builder(directories).build()
}
```

The verifier requires `created`, `expires`, `keyid` and `tag="web-bot-auth"`, coverage of
`@authority` (or `@target-uri`) and the signature's `Signature-Agent` member, and accepts
`created`/`expires` within 60 s of clock skew. Directories are cached for their HTTP cache
lifetime, bounded to 60 s–24 h. The RFC 9421 test key is rejected unless
`allow_test_keys(true)`. The verifier assigns no trust tier; that is the AWP edge's decision.
See the [Web Bot Auth guide](../docs/official_docs/security/web-bot-auth.md).

## Credential Brokers

Both brokers implement `adk_core::CredentialBroker` and mint a `ScopedToken` whose audience is
the requested resource (RFC 8707); `ScopedToken::authorize_for` refuses any other URL.

| Broker | Feature | Token source |
|--------|---------|--------------|
| `SecretCredentialBroker` | none | A `SecretProvider` secret bound to an exact `(AgentId, resource)` pair and its granted scopes |
| `TokenExchangeBroker` | `token-exchange` | RFC 8693 exchange of the `IdentitySource` credential, or, with `on_behalf_of(SubjectTokenSource)`, of the run's subject's token with the credential as `actor_token`; tokens are cached per subject, delegation chain, agent, resource, and scopes; optional RFC 7009 revocation |

See [Agent Identity](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/security/agent-identity.md).

## Changes in 3.0

| Change | Migration |
|--------|-----------|
| A guarded tool whose audit sink fails is refused with `auth.audit_failed` (`AuditFailureMode::Block`) | `with_audit_failure_mode(AuditFailureMode::Warn)` keeps running when the sink is down |
| `AuditEvent` gains `agent_id` and `on_behalf_of` | Set them in struct literals |
| `AuditFailureMode` moves to `adk_core::audit_log` and is re-exported here | — |
| Signed audit log, credential brokers, token exchange, and Web Bot Auth are new | — |
| `PostgresAuditSink` writes `agent_id` and `on_behalf_of` columns | Call `migrate()` once before the first write; until then inserts fail and guarded tools are refused |
| `PostgresAuditSink` uses SQLx 0.9 | See SQLx compatibility below |
| HTTP requests to IdPs and token endpoints have connect and request timeouts | — |

See the [2.x to 3.0 migration guide](../docs/official_docs/migration/2.x-to-3.0.md) and the [changelog](../CHANGELOG.md) for the full list.

## Error Types

| Type | When |
|------|------|
| `AccessDenied` | RBAC check fails (user lacks permission) |
| `AuthError` | Role not found, audit sink failure |
| `ScopeDenied` | Scope check fails (missing required scopes) |
| `TokenError` | JWT validation failure (expired, bad signature, missing claims) |
| `SsoError` | Token validation, access denied, or a grant the audit sink failed to record in SSO flow |
| `web_bot_auth::WebBotAuthError` | A Web Bot Auth signature is malformed, invalid, or unverifiable (`is_unverified`) |

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
- JWKS refreshes and OIDC discovery give up after 10 seconds, and `TokenExchangeBroker` requests after 30 seconds, each with a 5-second connect timeout.
- `OidcProvider::from_discovery()` rejects discovery documents whose `issuer` does not match the requested issuer URL.
- Token revocation and blacklist checks are not built in. If you need immediate revocation, enforce it in a custom `TokenValidator` or request extractor.
- `JwtValidator` rejects symmetric algorithms (HS256/HS384/HS512) and EdDSA — only RSA and EC algorithms are supported with JWKS-based validation.
- Every validator refuses a token without an `iss` or `aud` claim. `JwtValidatorBuilder::without_audience()` is the only way to accept tokens that name no audience; pin an audience whenever the issuer sets one.

## SQLx compatibility

ADK-Rust 3.0 SQL backends use SQLx 0.9. SQLx types appear in this crate's public
API — `PostgresAuditSink::from_pool` and `PostgresAuditSink::pool` — so an application that passes in its own pool must depend on
SQLx 0.9 too. Applications that let ADK open its own connections need no SQLx
dependency. The [SQLx 0.9 migration guide](../docs/official_docs/migration/sqlx-0.9.md)
lists who is affected and what changed, and its
[TLS behaviour](../docs/official_docs/migration/sqlx-0.9.md#tls-behaviour) section
covers PostgreSQL certificate verification.

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
