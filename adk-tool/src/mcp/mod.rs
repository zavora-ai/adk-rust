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

/// Selects how the client establishes its MCP lifecycle, and the trait that
/// applies the selection.
///
/// The default connection path uses [`ClientLifecycleMode::Initialize`], the
/// handshake every MCP server has understood since `2024-11-05`. Build a client
/// with [`ClientServiceExt::serve_with_lifecycle`] to choose a different one,
/// then hand it to [`McpToolset::new`].
///
/// | Mode | Sends first | On a server that predates `2026-07-28` |
/// |------|-------------|----------------------------------------|
/// | `Initialize` (default) | `initialize` | Works |
/// | `Auto` | `server/discover` | Falls back **only** when the server answers `METHOD_NOT_FOUND` |
/// | `Discover` | `server/discover` | Fails — there is no fallback |
///
/// # Choosing a mode
///
/// Prefer the default. A `2026-07-28` server still answers `initialize`, so the
/// default already reaches both generations of server.
///
/// `Auto` buys the stateless per-request path against new servers, at a cost:
/// a server that answers an unknown method with anything other than
/// `METHOD_NOT_FOUND` — `INVALID_REQUEST`, an internal error, or a closed
/// connection — fails the connection outright. rmcp also applies no timeout to
/// the probe, so a server that ignores `server/discover` instead of refusing it
/// leaves the connection waiting.
///
/// `Discover` never falls back. Use it only for servers you control and know to
/// support `2026-07-28`.
///
/// # Example
///
/// ```no_run
/// use adk_tool::mcp::{
///     AdkClientHandler, AutoDeclineElicitationHandler, ClientLifecycleMode,
///     ClientServiceExt, McpToolset,
/// };
/// use rmcp::model::ProtocolVersion;
/// use rmcp::transport::TokioChildProcess;
/// use std::sync::Arc;
/// use tokio::process::Command;
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let transport = TokioChildProcess::new(Command::new("my-mcp-server"))?;
/// let client = AdkClientHandler::new(Arc::new(AutoDeclineElicitationHandler))
///     .serve_with_lifecycle(
///         transport,
///         ClientLifecycleMode::Auto {
///             preferred_versions: vec![ProtocolVersion::V_2026_07_28],
///             legacy_version: Some(ProtocolVersion::V_2025_11_25),
///         },
///     )
///     .await?;
/// let toolset = McpToolset::new(client);
/// # Ok(())
/// # }
/// ```
pub use rmcp::{ClientLifecycleMode, ClientServiceExt};
