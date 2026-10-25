//! # adk-awp
//!
//! Agentic Web Protocol (AWP) implementation for ADK-Rust.
//!
//! This crate provides AWP protocol infrastructure including:
//!
//! - **Configuration**: TOML-based business context loading with hot-reload
//! - **Discovery**: Auto-generated discovery documents from business context
//! - **Manifest**: JSON-LD capability manifest builder
//! - **Detection**: Requester type detection (human vs. agent)
//! - **Trust**: Trust level assignment from request headers
//! - **Rate Limiting**: Per-trust-level sliding window rate limiter
//! - **Consent**: Consent capture, check, and revocation
//! - **Events**: Event subscriptions delivered as signed webhooks with retries and an
//!   egress guard that refuses internal callback URLs
//! - **Health**: Health state machine (Healthy/Degrading/Degraded)
//! - **Middleware**: AWP version negotiation
//! - **Router**: Safe public routes and separately composable management routes
//! - **A2A dispatch**: Application-provided handling with a fail-closed default
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use adk_awp::{BusinessContextLoader, build_manifest, generate_discovery_document};
//!
//! # fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let loader = BusinessContextLoader::from_file("business.toml".as_ref())?;
//! let ctx = loader.load();
//! let discovery = generate_discovery_document(&ctx);
//! let manifest = build_manifest(&ctx);
//! # let _ = (discovery, manifest);
//! # Ok(())
//! # }
//! ```

pub mod a2a;
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

pub use a2a::AwpA2aHandler;
pub use config::{AwpConfigError, business_context_to_toml};
pub use consent::{ConsentService, FileConsentService, InMemoryConsentService};
pub use detect::detect_requester_type;
pub use discovery::generate_discovery_document;
pub use events::{
    AwpEvent, EventSubscription, EventSubscriptionService, InMemoryEventSubscriptionService,
};
// Re-exported for existing callers; each use outside this crate still warns (spec 3.5 §10 X5).
#[allow(deprecated)]
pub use events::{sign_payload, verify_signature};
pub use health::{HealthState, HealthStateMachine, HealthStateSnapshot};
pub use loader::BusinessContextLoader;
pub use manifest::build_manifest;
pub use rate_limit::{InMemoryRateLimiter, RateLimitConfig, RateLimiter};
pub use router::{awp_management_routes, awp_public_routes, awp_routes};
pub use state::{AwpState, AwpStateBuilder};
pub use trust::{DefaultTrustAssigner, TrustLevelAssigner};
