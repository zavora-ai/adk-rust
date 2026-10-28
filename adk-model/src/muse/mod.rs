//! Meta Muse provider implementation for ADK.
//!
//! This module provides support for Meta's Muse Spark models through the Meta
//! Model API (`https://api.meta.ai/v1`), which is drop-in compatible with the
//! OpenAI Chat Completions format:
//! - `muse-spark-1.3` ([`MuseConfig::spark_1_3`], the default, recommended for new work)
//! - `muse-spark-1.2` ([`MuseConfig::spark_1_2`], the previous version)
//! - `muse-spark-1.1` ([`MuseConfig::spark_1_1`], the original version)
//!
//! Any other model the API serves works through [`MuseConfig::new`], including
//! the `-contributor` checkpoints. Contributor pricing is discounted in exchange
//! for permission to train on request content, so no preset selects one.
//!
//! # Features
//!
//! - **Agentic Workflows**: Spark 1.3 is tuned for tool calling and multi-step agents
//! - **Tool Calling**: Full function/tool calling support
//! - **Streaming**: Real-time streaming responses
//! - **Long Context**: 1,048,576-token context window across the Spark line
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_model::muse::{MuseClient, MuseConfig};
//!
//! // Muse Spark 1.3, the recommended default
//! let client = MuseClient::new(MuseConfig::spark_1_3(
//!     std::env::var("META_API_KEY").unwrap()
//! ))?;
//!
//! // Muse Spark 1.2, the previous version
//! let previous = MuseClient::new(MuseConfig::spark_1_2(
//!     std::env::var("META_API_KEY").unwrap()
//! ))?;
//! ```

mod client;
mod config;
mod convert;

pub use client::MuseClient;
pub use config::{MUSE_API_BASE, MuseConfig};
