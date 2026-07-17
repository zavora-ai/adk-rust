mod auth;
mod elicitation;
mod http;
pub mod manager;
mod reconnect;
mod resource_notifications;
mod task;
mod toolset;

pub use auth::{AuthError, McpAuth, OAuth2Config};
pub use elicitation::{AdkClientHandler, AutoDeclineElicitationHandler, ElicitationHandler};
pub use http::McpHttpClientBuilder;
pub use manager::{McpServerConfig, McpServerManager, RestartPolicy, ServerStatus};
pub use reconnect::{
    ConnectionFactory, ConnectionRefresher, RefreshConfig, RetryResult, SimpleClient,
    should_refresh_connection,
};
pub use resource_notifications::ResourceNotificationHandler;
pub use task::{CreateTaskResult, McpTaskConfig, TaskError, TaskInfo, TaskStatus};
pub use toolset::{McpToolset, ToolFilter};

/// The official Rust MCP SDK version used by ADK-Rust.
///
/// Re-exporting it keeps advanced transports, server authoring, extension
/// metadata, and protocol types on the same major version as [`McpToolset`].
pub use rmcp;

// Re-export commonly used catalog types from rmcp for public API consumers.
pub use rmcp::model::{
    CompletionContext, CompletionInfo, GetPromptResult, Prompt, Resource, ResourceContents,
    ResourceTemplate,
};
