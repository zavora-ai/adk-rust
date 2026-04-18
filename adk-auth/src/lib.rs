//! # adk-auth
//!
//! Access control and authentication for ADK-Rust.
//!
//! ## Overview
//!
//! This crate provides enterprise-grade access control:
//!
//! - [`Permission`] - Tool and agent permissions
//! - [`Role`] - Role with allow/deny rules
//! - [`AccessControl`] - Permission checking
//! - [`ScopeGuard`] - Declarative scope-based tool authorization
//! - [`AuditSink`] - Audit logging trait
//!
//! ## Features
//!
//! - `sso` - Enable SSO/OAuth/OIDC support
//! - `auth-bridge` - Enable JWT request context extraction for `adk-server`
//! - `aws-secrets` - Enable AWS Secrets Manager provider
//! - `azure-keyvault` - Enable Azure Key Vault provider
//! - `gcp-secrets` - Enable GCP Secret Manager provider
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use adk_auth::{Permission, Role, AccessControl};
//!
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
//! ```

mod access_control;
mod audit;
mod error;
mod middleware;
mod permission;
mod role;
pub mod scope;

#[cfg(feature = "auth-bridge")]
pub mod auth_bridge;

// SSO module (feature-gated)
#[cfg(feature = "sso")]
pub mod sso;

// Cloud secret manager integration
pub mod secrets;

pub use access_control::{AccessControl, AccessControlBuilder};
pub use audit::{AuditEvent, AuditEventType, AuditOutcome, AuditSink, FileAuditSink};
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
