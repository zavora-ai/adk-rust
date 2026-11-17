//! # adk-action
//!
//! Shared action node types for ADK-Rust graph workflows.
//!
//! This crate provides the type definitions, error types, and variable interpolation
//! utilities for deterministic, non-LLM workflow operations. `adk-graph` is the runtime
//! that executes them.
//!
//! ## Contents
//!
//! - **types** — All 14 action node config structs, `StandardProperties`, and the
//!   `ActionNodeConfig` tagged union enum.
//! - **error** — `ActionError` enum with `thiserror` for all action node failure modes.
//! - **interpolation** — `interpolate_variables()` and `get_nested_value()` for
//!   `{{variable}}` template resolution.

pub mod error;
pub mod interpolation;
pub mod types;

pub use error::ActionError;
pub use interpolation::{get_nested_value, interpolate_variables};
pub use types::*;
