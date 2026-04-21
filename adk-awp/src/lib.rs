//! # adk-awp
//!
//! Agentic Web Protocol (AWP) implementation for ADK-Rust.
//!
//! This crate provides the full AWP protocol implementation including:
//!
//! - **Configuration**: TOML-based business context loading with hot-reload
//! - **Discovery**: Auto-generated discovery documents from business context
//! - **Manifest**: JSON-LD capability manifest builder
//! - **Detection**: Requester type detection (human vs. agent)
//! - **Trust**: Trust level assignment from request headers
//! - **Rate Limiting**: Per-trust-level sliding window rate limiter
//! - **Consent**: Consent capture, check, and revocation
//! - **Events**: Event subscription system with HMAC-SHA256 webhook signing
//! - **Health**: Health state machine (Healthy/Degrading/Degraded)
//! - **Middleware**: AWP version negotiation
//! - **Router**: Axum route registration for all AWP endpoints
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use adk_awp::{BusinessContextLoader, generate_discovery_document, build_manifest, awp_routes};
//!
//! let loader = BusinessContextLoader::from_file("business.toml".as_ref())?;
//! let ctx = loader.load();
//! let discovery = generate_discovery_document(&ctx);
//! let manifest = build_manifest(&ctx);
//! ```

pub mod config;
pub mod consent;
pub mod detect;
pub mod discovery;
pub mod error_response;
pub mod events;
pub mod handlers;
pub mod health;
pub mod loader;
pub mod manifest;
pub mod middleware;
pub mod rate_limit;
pub mod router;
pub mod state;
pub mod trust;

pub use config::{AwpConfigError, business_context_to_toml};
pub use consent::{ConsentService, InMemoryConsentService};
pub use detect::detect_requester_type;
pub use discovery::generate_discovery_document;
pub use events::{
    AwpEvent, EventSubscription, EventSubscriptionService, InMemoryEventSubscriptionService,
    sign_payload, verify_signature,
};
pub use health::{HealthState, HealthStateMachine, HealthStateSnapshot};
pub use loader::BusinessContextLoader;
pub use manifest::build_manifest;
pub use rate_limit::{InMemoryRateLimiter, RateLimitConfig, RateLimiter};
pub use router::awp_routes;
pub use state::AwpState;
pub use trust::{DefaultTrustAssigner, TrustLevelAssigner};
