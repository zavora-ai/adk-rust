//! Amazon Bedrock provider implementation for ADK.
//!
//! Provides access to Amazon Bedrock models (Claude, Llama, Mistral, etc.)
//! via the AWS SDK Converse API with IAM/STS authentication. Requires the
//! `bedrock` feature flag.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use adk_model::bedrock::{BedrockClient, BedrockConfig};
//!
//! let config = BedrockConfig::new("us-east-1", "us.anthropic.claude-sonnet-4-6");
//! let client = BedrockClient::new(config).await?;
//! ```
//!
//! # Authentication
//!
//! Bedrock uses AWS IAM credentials loaded from the standard credential chain
//! (environment variables, `~/.aws/credentials`, IMDS, etc.). No API key is needed.
//!
//! # Inference Profiles
//!
//! Newer models require cross-region inference profile IDs (prefixed with `us.`
//! or `global.`) instead of raw model IDs. Use `aws bedrock list-inference-profiles`
//! to discover available profiles.

mod client;
mod config;
pub(crate) mod convert;

pub use client::BedrockClient;
pub use config::BedrockConfig;
