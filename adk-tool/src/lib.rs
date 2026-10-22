//! # adk-tool
//!
//! Tool system for ADK agents: typed Rust tools, toolset composition, hosted
//! provider tools, and Model Context Protocol clients and servers.
//!
//! ## Overview
//!
//! This crate provides the tool infrastructure for ADK agents:
//!
//! - [`macro@tool`] - Turn an async function into a [`Tool`] whose arguments are
//!   checked against the derived schema before it runs
//! - [`FunctionTool`] - Create tools from async Rust functions
//! - [`StatefulTool`] - Share `Arc<S>` state with a tool handler
//! - [`AgentTool`] - Use agents as callable tools for composition
//! - [`GoogleSearchTool`] and the other [`builtin`] wrappers - Provider-hosted tools,
//!   each declaring its [`ToolEffect`]
//! - `McpToolset` - MCP tools, resources, prompts, completion, elicitation,
//!   subscriptions, and negotiated tasks with the `mcp` feature
//! - `McpServerManager` - Dynamic local MCP server registry, process lifecycle,
//!   persistence, health monitoring, and bounded restart with the `mcp` feature
//! - [`BasicToolset`], [`FilteredToolset`], [`MergedToolset`], [`PrefixedToolset`] -
//!   Group, filter, combine, and namespace toolsets
//! - [`ToolArgumentValidator`] - Check hand-written tools' arguments against a JSON Schema
//! - [`ExitLoopTool`] - Control flow for loop agents
//! - [`LoadArtifactsTool`] - Inject binary artifacts into context
//!
//! ## Quick Start
//!
//! ```rust
//! use adk_core::{Result, ToolContext};
//! use adk_tool::FunctionTool;
//! use schemars::JsonSchema;
//! use serde::Serialize;
//! use serde_json::{Value, json};
//! use std::sync::Arc;
//!
//! #[derive(Serialize, JsonSchema)]
//! struct WeatherParams {
//!     /// The city to get weather for
//!     city: String,
//! }
//!
//! async fn get_weather(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
//!     let city = args["city"].as_str().unwrap_or("Unknown");
//!     Ok(json!({
//!         "city": city,
//!         "temperature": 22,
//!         "condition": "sunny"
//!     }))
//! }
//!
//! let tool = FunctionTool::new("get_weather", "Get current weather for a city", get_weather)
//!     .with_parameters_schema::<WeatherParams>();
//! ```
//!
//! ## MCP Integration
//!
//! Connect to MCP servers for external tools (requires the `mcp` feature):
//!
//! ```rust,no_run
//! # #[cfg(feature = "mcp")]
//! # async fn connect() -> Result<(), Box<dyn std::error::Error>> {
//! use adk_tool::{
//!     McpToolset,
//!     mcp::rmcp::{ServiceExt, transport::TokioChildProcess},
//! };
//! use tokio::process::Command;
//!
//! let mut command = Command::new("/opt/company/bin/workspace-mcp");
//! command.arg("--stdio").arg("--root").arg("/srv/workspace");
//! let client = ().serve(TokioChildProcess::new(command)?).await?;
//!
//! let toolset = McpToolset::new(client);
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]

mod agent_tool;
/// Argument validation against a tool's parameters schema.
mod arguments;
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

#[cfg(feature = "example-store")]
pub mod example_store;

#[cfg(feature = "vertex-agent-registry")]
pub mod vertex;

pub use adk_core::{
    AdkError, ErrorCategory, ErrorComponent, Result, TOOL_INVALID_ARGUMENTS_CODE, Tool,
    ToolContext, ToolEffect, Toolset,
};
pub use adk_rust_macros::tool;

// Re-export async_trait so the #[tool] macro's generated code can reference it
// without requiring users to add async-trait as a direct dependency.
pub use agent_tool::{
    AgentTool, AgentToolConfig, AgentToolFailureMode, AgentToolSessionSnapshot,
    AgentToolStateMergePolicy,
};
pub use arguments::{
    TOOL_INVALID_PARAMETERS_SCHEMA_CODE, ToolArgumentValidator, close_object_schemas,
    coerce_integral_floats, model_facing_schema,
};
pub use async_trait::async_trait;
pub use builtin::{
    AnthropicBashTool20241022, AnthropicBashTool20250124, AnthropicTextEditorTool20250124,
    AnthropicTextEditorTool20250429, AnthropicTextEditorTool20250728, BypassBuiltinTool,
    BypassMultiToolsLimit, DeepSeekWebSearchTool, ExitLoopTool, GeminiCodeExecutionTool,
    GeminiComputerEnvironment, GeminiComputerUseTool, GeminiFileSearchTool, GoogleMapsContext,
    GoogleMapsTool, GoogleSearchTool, LoadArtifactsTool, OpenAIApplyPatchTool,
    OpenAIApproximateLocation, OpenAICodeInterpreterTool, OpenAIComputerEnvironment,
    OpenAIComputerUseTool, OpenAIFileSearchTool, OpenAIImageGenerationTool, OpenAILocalShellTool,
    OpenAIMcpTool, OpenAIShellTool, OpenAIWebSearchTool, UrlContextTool, WebSearchTool,
    WebSearchUserLocation,
};
#[cfg(feature = "example-store")]
pub use example_store::{ExampleStoreClient, ExampleStoreConfig, ExampleStoreProvider};
pub use function_tool::FunctionTool;
#[cfg(feature = "mcp")]
pub use mcp::{
    AutoDeclineElicitationHandler, ElicitationHandler, McpAuth, McpDiscoveryLimits,
    McpHttpClientBuilder, McpSchemaLimits, McpServerManager, McpTaskConfig, McpToolset,
    OAuth2Config, Resource, ResourceContents, ResourceNotificationHandler, ResourceTemplate,
};
pub use simple_context::SimpleToolContext;
pub use stateful_tool::StatefulTool;
pub use toolset::{
    BasicToolset, FilteredToolset, MergedToolset, PrefixedToolset, string_predicate,
};
#[cfg(feature = "vertex-agent-registry")]
pub use vertex::agent_registry::{AgentRegistryClient, AgentRegistryConfig, AgentSearchTool};

#[cfg(feature = "code")]
pub use code_execution::CodeTool;

#[cfg(feature = "code")]
pub use code_execution::FrontendCodeTool;

#[cfg(feature = "code")]
pub use code_execution::JavaScriptCodeTool;

#[cfg(feature = "code")]
pub use code_execution::PythonCodeTool;

#[cfg(feature = "code")]
pub use code_execution::MontyPythonCodeTool;

#[cfg(feature = "code-embedded-python")]
pub use code_execution::MontyPythonCodeToolBuilder;
