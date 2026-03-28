//! Agent Commerce Protocol adapter scaffolding.
//!
//! This module targets ACP stable `2026-01-30`. Unreleased ACP discovery,
//! webhook, and delegated-authentication extensions remain feature-gated behind
//! `acp-experimental`.

mod mapper;
mod server;
mod types;
mod verification;

#[cfg(feature = "acp-experimental")]
#[cfg_attr(docsrs, doc(cfg(feature = "acp-experimental")))]
pub mod experimental;

pub use server::{AcpContextTemplate, AcpRouterBuilder};
pub use verification::{
    AcpVerificationConfig, DetachedSignatureVerifier, IdempotencyDecision, IdempotencyMode,
    IdempotencyStore, InMemoryIdempotencyStore, StoredIdempotentResponse,
};

#[cfg(feature = "acp-experimental")]
pub use experimental::AcpExperimentalRouterBuilder;
