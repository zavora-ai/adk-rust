//! SSO and OAuth/OIDC integration for adk-auth.
//!
//! This module provides JWT validation, OIDC provider support, and SSO integration.
//!
//! # Features
//!
//! Enable the `sso` feature to use these modules:
//!
//! ```toml
//! [dependencies]
//! adk-auth = { version = "0.1", features = ["sso"] }
//! ```
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use adk_auth::sso::{GoogleProvider, TokenValidator};
//!
//! let provider = GoogleProvider::new("your-client-id");
//! let claims = provider.validate(token).await?;
//! println!("User: {}", claims.sub);
//! ```

mod claims;
mod error;
mod jwks;
mod mapper;
mod sso_access;
mod validator;

pub use claims::{Audience, TokenClaims};
pub use error::TokenError;
pub use jwks::JwksCache;
pub use mapper::{ClaimsMapper, ClaimsMapperBuilder, UserIdClaim};
pub use sso_access::{SsoAccessControl, SsoAccessControlBuilder, SsoError};
pub use validator::{JwtValidator, JwtValidatorBuilder, TokenValidator};

// Re-export providers when available
#[cfg(feature = "sso")]
mod providers;
#[cfg(feature = "sso")]
pub use providers::*;
