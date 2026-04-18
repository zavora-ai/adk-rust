//! Native Slack toolset for ADK agents.
//!
//! Provides tools for sending messages, reading channels, adding reactions,
//! and listing threads via the Slack API.
//!
//! Enable with the `slack` feature flag.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use adk_tool::slack::SlackToolset;
//!
//! // Direct token
//! let toolset = SlackToolset::new("xoxb-your-bot-token");
//!
//! // Or via secret provider
//! let toolset = SlackToolset::from_secret("slack-bot-token");
//! ```

mod tools;
mod toolset;

pub use toolset::{SlackToolset, TokenSource};
