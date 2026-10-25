//! # adk-auth
//!
//! Access control and authentication for ADK-Rust.
//!
//! ## Overview
//!
//! This crate provides access control for agents and tools:
//!
//! - [`Permission`] - Tool and agent permissions
//! - [`Role`] - Role with allow/deny rules
//! - [`AccessControl`] - Permission checking
//! - [`AuthMiddleware`] / [`ProtectedTool`] - Role-based tool protection
//! - [`ScopeGuard`] - Declarative scope-based tool authorization
//! - [`AuditSink`] - Audit logging trait; [`FileAuditSink`] writes JSONL
//! - [`SecretCredentialBroker`] - Audience-bound tokens for an agent's outbound calls
//!
//! ## Features
//!
//! - `sso` - Enable SSO/OAuth/OIDC support
//! - `auth-bridge` - Enable JWT request context extraction for `adk-server`
//! - `aws-secrets` - Enable AWS Secrets Manager provider
//! - `azure-keyvault` - Enable Azure Key Vault provider
//! - `gcp-secrets` - Enable GCP Secret Manager provider
//! - `postgres-audit` - PostgreSQL audit sink
//! - `otlp-audit` - OpenTelemetry audit sink
//! - `signed-audit` - Signed, Merkle-hashed audit log (`signed_audit`)
//! - `token-exchange` - RFC 8693 token exchange broker (`TokenExchangeBroker`)
//! - `web-bot-auth` - Web Bot Auth request signatures, signer and verifier (`web_bot_auth`)
//!
//! ## Quick Start
//!
//! ```rust
//! use adk_auth::{AccessControl, Permission, Role};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let admin = Role::new("admin")
//!     .allow(Permission::AllTools)
//!     .allow(Permission::AllAgents);
//!
//! let user = Role::new("user")
//!     .allow(Permission::Tool("search".into()))
//!     .deny(Permission::Tool("code_exec".into()));
//!
//! let ac = AccessControl::builder()
//!     .role(admin)
//!     .role(user)
//!     .assign("alice@example.com", "admin")
//!     .build()?;
//!
//! ac.check("alice@example.com", &Permission::AllTools)?;
//! assert!(ac.check("bob@example.com", &Permission::AllTools).is_err());
//! # Ok(())
//! # }
//! ```

mod access_control;
mod audit;
mod broker;
mod error;
mod middleware;
mod permission;
mod role;
pub mod scope;

#[cfg(feature = "auth-bridge")]
pub mod auth_bridge;

#[cfg(any(feature = "sso", feature = "token-exchange"))]
mod http;

// SSO module (feature-gated)
#[cfg(feature = "sso")]
pub mod sso;

// Cloud secret manager integration
pub mod secrets;

#[cfg(feature = "token-exchange")]
mod token_exchange;

// Enterprise audit sinks (feature-gated)
#[cfg(feature = "otlp-audit")]
pub mod audit_otlp;
#[cfg(feature = "postgres-audit")]
pub mod audit_postgres;
#[cfg(feature = "signed-audit")]
pub mod signed_audit;
#[cfg(feature = "web-bot-auth")]
pub mod web_bot_auth;

pub use access_control::{AccessControl, AccessControlBuilder};
pub use audit::{
    AuditEvent, AuditEventType, AuditFailureMode, AuditFilter, AuditOutcome, AuditSink,
    FileAuditSink, InMemoryAuditSink,
};
pub use broker::SecretCredentialBroker;
pub use error::{AccessDenied, AuthError};
pub use middleware::{AuthMiddleware, ProtectedTool, ProtectedToolDyn, ToolExt};
pub use permission::Permission;
pub use role::Role;
pub use scope::{
    ContextScopeResolver, ScopeDenied, ScopeGuard, ScopeResolver, ScopeToolExt, ScopedTool,
    ScopedToolDyn, StaticScopeResolver, check_scopes,
};

#[cfg(feature = "auth-bridge")]
pub use auth_bridge::{JwtRequestContextExtractor, JwtRequestContextExtractorBuilder};

#[cfg(feature = "token-exchange")]
pub use token_exchange::{SubjectToken, SubjectTokenSource, TokenExchangeBroker};

#[cfg(feature = "otlp-audit")]
pub use audit_otlp::OtlpAuditSink;
#[cfg(feature = "postgres-audit")]
pub use audit_postgres::PostgresAuditSink;
