//! # adk-model
//!
//! LLM model integrations for ADK (Gemini, etc.).
//!
//! ## Overview
//!
//! This crate provides LLM implementations for ADK agents. Currently supports:
//!
//! - [`GeminiModel`] - Google's Gemini models (2.0 Flash, Pro, etc.)
//! - [`MockLlm`] - Mock LLM for testing
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use adk_model::GeminiModel;
//! use std::sync::Arc;
//!
//! let api_key = std::env::var("GOOGLE_API_KEY").unwrap();
//! let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp").unwrap();
//!
//! // Use with an agent
//! // let agent = LlmAgentBuilder::new("assistant")
//! //     .model(Arc::new(model))
//! //     .build()?;
//! ```
//!
//! ## Supported Models
//!
//! | Model | Description |
//! |-------|-------------|
//! | `gemini-2.0-flash-exp` | Fast, efficient model (recommended) |
//! | `gemini-1.5-pro` | Most capable model |
//! | `gemini-1.5-flash` | Balanced speed/capability |
//!
//! ## Features
//!
//! - Async streaming with backpressure
//! - Tool/function calling support
//! - Multimodal input (text, images, audio, video, PDF)
//! - Generation configuration (temperature, top_p, etc.)

pub mod gemini;
pub mod mock;

pub use gemini::GeminiModel;
pub use mock::MockLlm;
