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

#[cfg(feature = "code")]
pub mod code_execution;

pub use adk_core::{AdkError, Result, Tool, ToolContext, Toolset};
pub use adk_rust_macros::tool;

// Re-export async_trait so the #[tool] macro's generated code can reference it
// without requiring users to add async-trait as a direct dependency.
pub use agent_tool::{AgentTool, AgentToolConfig};
pub use async_trait::async_trait;
pub use builtin::{
    AnthropicBashTool20241022, AnthropicBashTool20250124, AnthropicTextEditorTool20250124,
    AnthropicTextEditorTool20250429, AnthropicTextEditorTool20250728, ExitLoopTool,
    GeminiCodeExecutionTool, GeminiComputerEnvironment, GeminiComputerUseTool,
    GeminiFileSearchTool, GoogleMapsContext, GoogleMapsTool, GoogleSearchTool, LoadArtifactsTool,
    OpenAIApplyPatchTool, OpenAIApproximateLocation, OpenAICodeInterpreterTool,
    OpenAIComputerEnvironment, OpenAIComputerUseTool, OpenAIFileSearchTool,
    OpenAIImageGenerationTool, OpenAILocalShellTool, OpenAIMcpTool, OpenAIShellTool,
    OpenAIWebSearchTool, UrlContextTool, WebSearchTool, WebSearchUserLocation,
};
pub use function_tool::FunctionTool;
pub use mcp::{
    AutoDeclineElicitationHandler, ElicitationHandler, McpAuth, McpHttpClientBuilder,
    McpTaskConfig, McpToolset, OAuth2Config,
};
pub use toolset::{
    BasicToolset, FilteredToolset, MergedToolset, PrefixedToolset, string_predicate,
};

#[cfg(feature = "code")]
#[allow(deprecated)]
pub use code_execution::RustCodeTool;

#[cfg(feature = "code")]
pub use code_execution::CodeTool;

#[cfg(feature = "code")]
pub use code_execution::FrontendCodeTool;

#[cfg(feature = "code")]
pub use code_execution::JavaScriptCodeTool;

#[cfg(feature = "code")]
pub use code_execution::PythonCodeTool;
