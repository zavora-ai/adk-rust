//! Server and router integration for payment protocols.
//!
//! This module provides helpers for exposing ACP stable `2026-01-30` and AP2
//! `v0.1-alpha` (`2026-03-22`) commerce endpoints from ADK deployments.
//!
//! # ACP routes
//!
//! When the `acp` feature is enabled, `AcpRouterBuilder` produces an Axum
//! router covering the stable checkout session lifecycle and delegated payment
//! endpoints. The `acp-experimental` feature adds discovery and webhook routes
//! via `AcpExperimentalRouterBuilder`.
//!
//! # AP2 integration
//!
//! AP2 flows are mandate-based and typically operate over A2A message or MCP
//! tool surfaces rather than dedicated HTTP routes. The `Ap2Adapter` in
//! `crate::protocol::ap2` can be wired into existing A2A or MCP server
//! helpers already present in `adk-server`.
//! The adapter governs every cart-mandate checkout and payment mandate with its
//! payment policies, spend ledger, and auditor, whichever surface delivers the
//! mandate. With the `ap2` feature, `crate::tools::ap2_payment_tool` exposes payment
//! mandate submission as an agent tool that an MCP server can serve.
//!
//! # Payment tools
//!
//! [`crate::tools::PaymentToolsetBuilder`] produces scope-protected tools
//! suitable for registration with `adk-tool` or any agent tool registry.

#[cfg(feature = "acp")]
#[cfg_attr(docsrs, doc(cfg(feature = "acp")))]
pub use crate::protocol::acp::{AcpContextTemplate, AcpRouterBuilder};

#[cfg(feature = "acp-experimental")]
#[cfg_attr(docsrs, doc(cfg(feature = "acp-experimental")))]
pub use crate::protocol::acp::AcpExperimentalRouterBuilder;
