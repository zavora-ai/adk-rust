mod auth;
mod elicitation;
mod http;
mod reconnect;
mod task;
mod toolset;

pub use auth::{AuthError, McpAuth, OAuth2Config};
pub use elicitation::{AdkClientHandler, AutoDeclineElicitationHandler, ElicitationHandler};
pub use http::McpHttpClientBuilder;
pub use reconnect::{
    ConnectionFactory, ConnectionRefresher, RefreshConfig, RetryResult, SimpleClient,
    should_refresh_connection,
};
pub use task::{CreateTaskResult, McpTaskConfig, TaskError, TaskInfo, TaskStatus};
pub use toolset::{McpToolset, ToolFilter};

// Re-export MCP resource types from rmcp for public API consumers.
pub use rmcp::model::{Resource, ResourceContents, ResourceTemplate};
