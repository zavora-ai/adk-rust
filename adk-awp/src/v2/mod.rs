//! The AWP 2.0 edge runtime (spec 3.5 §7.1–§7.4, §7.9).
//!
//! | Module | Contents | Spec |
//! |---|---|---|
//! | [`routes`] | The `/awp/v2/` routes and their state | §7.9 |
//! | [`negotiation`] | `AWP-Version`: the highest common major version | §7.2 |
//! | [`tier`] | [`TierAssigner`](tier::TierAssigner): tiers from verified credentials only | §7.3, A-I6 |
//! | [`capability`] | [`CapabilityTool`](capability::CapabilityTool): a capability as an `adk_core::Tool`; the registry and manifest | §7.4, D-4 |
//! | [`executor`] | [`CapabilityExecutor`](executor::CapabilityExecutor): the governed path every binding enters; held calls; exactly once | §7.4, A-I7, A-I8 |
//! | [`store`] | Edge stores with in-memory implementations and a conformance suite | §7.9 |
//! | [`migrate`] | `business.toml` to 2.0 site data and a draft charter | §8.1 |
//! | [`response`] | `application/awp+json` documents and the error envelope | §7.5 |
//!
//! The wire types are `awp_types::v2`.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//!
//! use adk_awp::v2::capability::{CapabilityCall, CapabilityError, CapabilityHandler, CapabilityRegistry};
//! use adk_awp::v2::routes::{EdgeState, awp_v2_routes};
//! use awp_types::{CapabilityEffect, CapabilityV2};
//! use serde_json::{Value, json};
//!
//! struct Search;
//!
//! #[async_trait::async_trait]
//! impl CapabilityHandler for Search {
//!     async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
//!         Ok(json!({ "query": call.input["q"], "results": [] }))
//!     }
//! }
//!
//! let mut registry = CapabilityRegistry::new("example.com");
//! registry
//!     .declare(CapabilityV2::new("search_site", "Search the site", CapabilityEffect::ReadOnly), Arc::new(Search))
//!     .unwrap();
//! let app: axum::Router = awp_v2_routes(EdgeState::builder(registry).build());
//! # let _ = app;
//! ```

pub mod capability;
pub mod executor;
pub mod migrate;
pub mod negotiation;
pub mod response;
pub mod routes;
pub mod store;
pub mod tier;
