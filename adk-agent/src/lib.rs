//! # adk-agent
//!
//! Agent implementations for ADK (LLM, Custom, Workflow agents).
//!
//! ## Overview
//!
//! This crate provides ready-to-use agent implementations:
//!
//! - [`LlmAgent`] - Core agent powered by LLM reasoning
//! - [`CustomAgent`] - Define custom logic without LLM
//! - [`SequentialAgent`] - Execute agents in sequence
//! - [`ParallelAgent`] - Execute agents concurrently
//! - [`LoopAgent`] - Iterate until exit condition
//! - [`ConditionalAgent`] - Branch based on conditions
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use adk_agent::LlmAgentBuilder;
//! use std::sync::Arc;
//!
//! // LLM Agent requires a model (from adk-model)
//! // let agent = LlmAgentBuilder::new("assistant")
//! //     .description("Helpful AI assistant")
//! //     .model(Arc::new(model))
//! //     .build()?;
//! ```
//!
//! ## Workflow Agents
//!
//! Combine agents for complex workflows:
//!
//! ```rust,ignore
//! // Sequential: A -> B -> C
//! let seq = SequentialAgent::new("pipeline", vec![a, b, c]);
//!
//! // Parallel: A, B, C simultaneously
//! let par = ParallelAgent::new("team", vec![a, b, c]);
//!
//! // Loop: repeat until exit
//! let loop_agent = LoopAgent::new("iterator", worker, 10);
//! ```
//!
//! ## Guardrails (optional)
//!
//! Enable the `guardrails` feature for input/output validation:
//!
//! ```rust,ignore
//! use adk_agent::{LlmAgentBuilder, guardrails::{GuardrailSet, ContentFilter, PiiRedactor}};
//!
//! let input_guardrails = GuardrailSet::new()
//!     .with(ContentFilter::harmful_content())
//!     .with(PiiRedactor::new());
//!
//! let agent = LlmAgentBuilder::new("assistant")
//!     .input_guardrails(input_guardrails)
//!     .build()?;
//! ```

mod custom_agent;
pub mod guardrails;
mod llm_agent;
pub mod tool_call_markup;
mod workflow;

pub use adk_core::Agent;
pub use custom_agent::{CustomAgent, CustomAgentBuilder};
pub use guardrails::GuardrailSet;
pub use llm_agent::{DEFAULT_MAX_ITERATIONS, LlmAgent, LlmAgentBuilder};
pub use tool_call_markup::{normalize_content, normalize_option_content};
pub use workflow::{
    ConditionalAgent, LlmConditionalAgent, LlmConditionalAgentBuilder, LoopAgent, ParallelAgent,
    SequentialAgent,
};
