//! # adk-tool
//!
//! Tool system for ADK agents: typed Rust tools, toolset composition, hosted
//! provider tools, and Model Context Protocol clients and servers.
//!
//! ## Overview
//!
//! This crate provides the tool infrastructure for ADK agents:
//!
//! - [`FunctionTool`] - Create tools from async Rust functions
//! - [`AgentTool`] - Use agents as callable tools for composition
//! - [`GoogleSearchTool`] - Web search via Gemini's grounding
//! - `McpToolset` - MCP tools, resources, prompts, completion, elicitation,
//!   subscriptions, and negotiated tasks with the `mcp` feature
//! - `McpServerManager` - Dynamic local MCP server registry, process lifecycle,
//!   persistence, health monitoring, and bounded restart with the `mcp` feature
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
//! use adk_tool::{
//!     McpToolset,
//!     mcp::rmcp::{ServiceExt, transport::TokioChildProcess},
//! };
//! use tokio::process::Command;
//!
//! let client = ().serve(TokioChildProcess::new(
//!     Command::new("/opt/company/bin/workspace-mcp")
//!         .arg("--stdio")
//!         .arg("--root")
//!         .arg("/srv/workspace")
//! )?).await?;
//!
//! let toolset = McpToolset::new(client);
//! ```

#![deny(missing_docs)]

mod agent_tool;
/// Built-in tool wrappers for Gemini, OpenAI, and Anthropic hosted tools.
pub mod builtin;
mod function_tool;
#[cfg(feature = "mcp")]
/// Model Context Protocol (MCP) clients, server SDK re-export, catalog APIs,
/// elicitation, tasks, HTTP transport, and dynamic local-server management.
pub mod mcp;
mod simple_context;
mod stateful_tool;
/// Toolset combinators: basic, filtered, merged, and prefixed toolsets.
pub mod toolset;

#[cfg(feature = "code")]
pub mod code_execution;

#[cfg(feature = "memory-tools")]
pub mod memory;

#[cfg(feature = "graph-memory-tools")]
pub use memory::{GraphMemoryToolset, RelateTool, RememberTool};

#[cfg(feature = "slack")]
pub mod slack;

#[cfg(feature = "bigquery")]
pub mod bigquery;

#[cfg(feature = "spanner")]
pub mod spanner;

#[cfg(feature = "mcp-sampling")]
pub mod sampling;

pub use adk_core::{AdkError, Result, Tool, ToolContext, Toolset};
pub use adk_rust_macros::tool;

// Re-export async_trait so the #[tool] macro's generated code can reference it
// without requiring users to add async-trait as a direct dependency.
pub use agent_tool::{AgentTool, AgentToolConfig};
pub use async_trait::async_trait;
pub use builtin::{
    AnthropicBashTool20241022, AnthropicBashTool20250124, AnthropicTextEditorTool20250124,
    AnthropicTextEditorTool20250429, AnthropicTextEditorTool20250728, BypassBuiltinTool,
    BypassMultiToolsLimit, ExitLoopTool, GeminiCodeExecutionTool, GeminiComputerEnvironment,
    GeminiComputerUseTool, GeminiFileSearchTool, GoogleMapsContext, GoogleMapsTool,
    GoogleSearchTool, LoadArtifactsTool, OpenAIApplyPatchTool, OpenAIApproximateLocation,
    OpenAICodeInterpreterTool, OpenAIComputerEnvironment, OpenAIComputerUseTool,
    OpenAIFileSearchTool, OpenAIImageGenerationTool, OpenAILocalShellTool, OpenAIMcpTool,
    OpenAIShellTool, OpenAIWebSearchTool, UrlContextTool, WebSearchTool, WebSearchUserLocation,
};
pub use function_tool::{FunctionTool, schema_for};
#[cfg(feature = "mcp")]
pub use mcp::{
    AutoDeclineElicitationHandler, ElicitationHandler, McpAuth, McpHttpClientBuilder,
    McpServerManager, McpTaskConfig, McpToolset, OAuth2Config, Resource, ResourceContents,
    ResourceNotificationHandler, ResourceTemplate,
};
pub use simple_context::SimpleToolContext;
pub use stateful_tool::StatefulTool;
pub use toolset::{
    BasicToolset, FilteredToolset, MergedToolset, PrefixedToolset, string_predicate,
};

#[cfg(feature = "code")]
pub use code_execution::CodeTool;

#[cfg(feature = "code")]
pub use code_execution::FrontendCodeTool;

#[cfg(feature = "code")]
pub use code_execution::JavaScriptCodeTool;

#[cfg(feature = "code")]
pub use code_execution::PythonCodeTool;
