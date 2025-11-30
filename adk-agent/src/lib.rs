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

mod custom_agent;
mod llm_agent;
mod workflow;

pub use adk_core::Agent;
pub use custom_agent::{CustomAgent, CustomAgentBuilder};
pub use llm_agent::{LlmAgent, LlmAgentBuilder};
pub use workflow::{ConditionalAgent, LoopAgent, ParallelAgent, SequentialAgent};
