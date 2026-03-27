#![cfg_attr(docsrs, feature(doc_cfg))]
#![allow(clippy::result_large_err)]

//! Protocol-neutral agentic commerce and payment orchestration for ADK-Rust.
//!
//! The initial crate scaffold tracks these protocol baselines:
//! - ACP stable `2026-01-30`
//! - ACP experimental extensions as an unreleased compatibility channel
//! - AP2 `v0.1-alpha` as of `2026-03-22`
//!
//! # Example
//!
//! ```
//! use adk_payments::{
//!     ACP_DELEGATE_AUTH_BASELINE, ACP_EXPERIMENTAL_CHANNEL, ACP_STABLE_BASELINE,
//!     AP2_ALPHA_BASELINE, AP2_ALPHA_BASELINE_DATE,
//! };
//!
//! assert_eq!(ACP_STABLE_BASELINE, "2026-01-30");
//! assert_eq!(ACP_EXPERIMENTAL_CHANNEL, "unreleased");
//! assert_eq!(ACP_DELEGATE_AUTH_BASELINE, "2026-01-28");
//! assert_eq!(AP2_ALPHA_BASELINE, "v0.1-alpha");
//! assert_eq!(AP2_ALPHA_BASELINE_DATE, "2026-03-22");
//! ```

/// ACP stable protocol baseline supported by this crate.
pub const ACP_STABLE_BASELINE: &str = "2026-01-30";

/// ACP experimental compatibility channel supported behind `acp-experimental`.
pub const ACP_EXPERIMENTAL_CHANNEL: &str = "unreleased";

/// ACP experimental delegated-authentication baseline version.
pub const ACP_DELEGATE_AUTH_BASELINE: &str = "2026-01-28";

/// AP2 protocol baseline supported by this crate.
pub const AP2_ALPHA_BASELINE: &str = "v0.1-alpha";

/// Date of the AP2 alpha research baseline tracked by this crate.
pub const AP2_ALPHA_BASELINE_DATE: &str = "2026-03-22";

pub mod auth;
pub mod domain;
pub mod guardrail;
pub mod journal;
pub mod kernel;
pub mod protocol;
pub mod server;
pub mod tools;
