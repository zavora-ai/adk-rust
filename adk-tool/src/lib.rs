//! # adk-tool
//!
//! Tool system for ADK agents (FunctionTool, MCP, Google Search, AgentTool).
//!
//! ## Overview
//!
//! This crate provides the tool infrastructure for ADK agents:
//!
//! - [`FunctionTool`] - Create tools from async Rust functions
//! - [`AgentTool`] - Use agents as callable tools for composition
//! - [`GoogleSearchTool`] - Web search via Gemini's grounding
//! - [`McpToolset`] - Model Context Protocol integration
//! - [`BasicToolset`] - Group multiple tools together
//! - [`ExitLoopTool`] - Control flow for loop agents
//! - [`LoadArtifactsTool`] - Inject binary artifacts into context
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use adk_tool::FunctionTool;
//! use adk_core::{ToolContext, Result};
//! use serde_json::{json, Value};
//! use std::sync::Arc;
//!
//! async fn get_weather(ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
//!     let city = args["city"].as_str().unwrap_or("Unknown");
//!     Ok(json!({
//!         "city": city,
//!         "temperature": 72,
//!         "condition": "sunny"
//!     }))
//! }
//!
//! let tool = FunctionTool::new(
//!     "get_weather",
//!     "Get current weather for a city",
//!     get_weather,
//! );
//! ```
//!
//! ## MCP Integration
//!
//! Connect to MCP servers for external tools:
//!
//! ```rust,ignore
//! use adk_tool::McpToolset;
//! use rmcp::{ServiceExt, transport::TokioChildProcess};
//!
//! let client = ().serve(TokioChildProcess::new(
//!     Command::new("npx")
//!         .arg("-y")
//!         .arg("@modelcontextprotocol/server-filesystem")
//!         .arg("/path/to/files")
//! )?).await?;
//!
//! let toolset = McpToolset::new(client);
//! ```

mod agent_tool;
pub mod builtin;
mod function_tool;
pub mod mcp;
pub mod toolset;

pub use adk_core::{Tool, ToolContext, Toolset};
pub use agent_tool::{AgentTool, AgentToolConfig};
pub use builtin::{ExitLoopTool, GoogleSearchTool, LoadArtifactsTool};
pub use function_tool::FunctionTool;
pub use mcp::McpToolset;
pub use toolset::{string_predicate, BasicToolset};
