//! Azure AI Inference provider for ADK.
//!
//! Provides access to models hosted on Azure AI Inference endpoints
//! (Cohere, Llama, Mistral, etc.) via the Azure AI REST API. Requires
//! the `azure-ai` feature flag.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use adk_model::azure_ai::{AzureAIClient, AzureAIConfig};
//!
//! let config = AzureAIConfig::new(
//!     "https://my-endpoint.eastus.inference.ai.azure.com",
//!     "my-api-key",
//!     "meta-llama-3.1-8b-instruct",
//! );
//! let client = AzureAIClient::new(config)?;
//! ```
//!
//! # Authentication
//!
//! Uses `api-key` header authentication with the Azure AI endpoint.

mod client;
mod config;
pub(crate) mod convert;

pub use client::AzureAIClient;
pub use config::AzureAIConfig;
