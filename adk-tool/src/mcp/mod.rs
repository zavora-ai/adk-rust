mod auth;
mod http;
mod reconnect;
mod task;
mod toolset;

pub use auth::{AuthError, McpAuth, OAuth2Config};
pub use http::McpHttpClientBuilder;
pub use reconnect::{
    ConnectionFactory, ConnectionRefresher, RefreshConfig, RetryResult, SimpleClient,
    should_refresh_connection,
};
pub use task::{CreateTaskResult, McpTaskConfig, TaskError, TaskInfo, TaskStatus};
pub use toolset::{McpToolset, ToolFilter};
