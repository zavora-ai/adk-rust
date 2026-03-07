//! MCP (Model Context Protocol) client integration for mistral.rs.
//!
//! This module provides configuration types and utilities for connecting
//! to MCP servers and discovering tools for use with mistral.rs models.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_mistralrs::{MistralRsConfig, McpClientConfig, McpServerConfig, McpServerSource};
//!
//! let mcp_config = McpClientConfig {
//!     servers: vec![
//!         McpServerConfig {
//!             name: "Filesystem Tools".to_string(),
//!             source: McpServerSource::Process {
//!                 command: "mcp-server-filesystem".to_string(),
//!                 args: vec!["--root".to_string(), "/tmp".to_string()],
//!                 work_dir: None,
//!                 env: None,
//!             },
//!             ..Default::default()
//!         },
//!     ],
//!     ..Default::default()
//! };
//!
//! let config = MistralRsConfig::builder()
//!     .model_source(ModelSource::huggingface("mistralai/Magistral-Small-2509"))
//!     .mcp_client(mcp_config)
//!     .build();
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use uuid::Uuid;

use crate::error::{MistralRsError, Result};

/// Supported MCP server transport sources.
///
/// Defines the different ways to connect to MCP servers, each optimized for
/// specific use cases and deployment scenarios.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum McpServerSource {
    /// HTTP-based MCP server using JSON-RPC over HTTP.
    ///
    /// Best for: Public APIs, RESTful services, servers behind load balancers.
    Http {
        /// Base URL of the MCP server (http:// or https://)
        url: String,
        /// Optional timeout in seconds for HTTP requests
        #[serde(default)]
        timeout_secs: Option<u64>,
        /// Optional headers to include in requests
        #[serde(default)]
        headers: Option<HashMap<String, String>>,
    },
    /// Local process-based MCP server using stdin/stdout communication.
    ///
    /// Best for: Local tools, development servers, sandboxed environments.
    Process {
        /// Command to execute (e.g., "mcp-server-filesystem")
        command: String,
        /// Arguments to pass to the command
        #[serde(default)]
        args: Vec<String>,
        /// Optional working directory for the process
        #[serde(default)]
        work_dir: Option<String>,
        /// Optional environment variables for the process
        #[serde(default)]
        env: Option<HashMap<String, String>>,
    },
    /// WebSocket-based MCP server for real-time bidirectional communication.
    ///
    /// Best for: Interactive applications, real-time data, low-latency requirements.
    WebSocket {
        /// WebSocket URL (ws:// or wss://)
        url: String,
        /// Optional timeout in seconds for connection establishment
        #[serde(default)]
        timeout_secs: Option<u64>,
        /// Optional headers for the WebSocket handshake
        #[serde(default)]
        headers: Option<HashMap<String, String>>,
    },
}

/// Configuration for MCP client integration.
///
/// This structure defines how the MCP client should connect to and manage
/// multiple MCP servers, including authentication, tool registration, and
/// execution policies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpClientConfig {
    /// List of MCP servers to connect to
    pub servers: Vec<McpServerConfig>,
    /// Whether to automatically register discovered tools with the model
    #[serde(default = "default_true")]
    pub auto_register_tools: bool,
    /// Timeout for individual tool execution in seconds
    #[serde(default)]
    pub tool_timeout_secs: Option<u64>,
    /// Maximum number of concurrent tool calls across all MCP servers
    #[serde(default)]
    pub max_concurrent_calls: Option<usize>,
}

/// Configuration for an individual MCP server.
///
/// Defines connection parameters, authentication, and tool management
/// settings for a single MCP server instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    /// Unique identifier for this server
    #[serde(default = "generate_uuid")]
    pub id: String,
    /// Human-readable name for this server
    pub name: String,
    /// Transport-specific connection configuration
    pub source: McpServerSource,
    /// Whether this server should be activated
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional prefix to add to all tool names from this server
    #[serde(default)]
    pub tool_prefix: Option<String>,
    /// Optional resource URI patterns this server provides
    #[serde(default)]
    pub resources: Option<Vec<String>>,
    /// Optional Bearer token for authentication
    #[serde(default)]
    pub bearer_token: Option<String>,
}

/// Information about a tool discovered from an MCP server.
#[derive(Debug, Clone)]
pub struct McpToolInfo {
    /// Name of the tool as reported by the MCP server
    pub name: String,
    /// Optional human-readable description of what the tool does
    pub description: Option<String>,
    /// JSON schema describing the tool's input parameters
    pub input_schema: serde_json::Value,
    /// ID of the server this tool comes from
    pub server_id: String,
    /// Display name of the server for logging and debugging
    pub server_name: String,
}

impl Default for McpClientConfig {
    fn default() -> Self {
        Self {
            servers: Vec::new(),
            auto_register_tools: true,
            tool_timeout_secs: None,
            max_concurrent_calls: Some(1),
        }
    }
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            id: generate_uuid(),
            name: String::new(),
            source: McpServerSource::Http { url: String::new(), timeout_secs: None, headers: None },
            enabled: true,
            tool_prefix: None,
            resources: None,
            bearer_token: None,
        }
    }
}

fn generate_uuid() -> String {
    Uuid::new_v4().to_string()
}

fn default_true() -> bool {
    true
}

impl McpClientConfig {
    /// Create a new empty MCP client configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an MCP client configuration with a single server.
    pub fn with_server(server: McpServerConfig) -> Self {
        Self { servers: vec![server], ..Default::default() }
    }

    /// Add a server to the configuration.
    pub fn add_server(mut self, server: McpServerConfig) -> Self {
        self.servers.push(server);
        self
    }

    /// Set the tool timeout in seconds.
    pub fn with_tool_timeout(mut self, timeout_secs: u64) -> Self {
        self.tool_timeout_secs = Some(timeout_secs);
        self
    }

    /// Set the maximum number of concurrent tool calls.
    pub fn with_max_concurrent_calls(mut self, max_calls: usize) -> Self {
        self.max_concurrent_calls = Some(max_calls);
        self
    }

    /// Disable automatic tool registration.
    pub fn without_auto_register(mut self) -> Self {
        self.auto_register_tools = false;
        self
    }

    /// Load MCP configuration from a JSON file.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the JSON configuration file
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let config = McpClientConfig::from_file("mcp-config.json")?;
    /// ```
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        if !path.exists() {
            return Err(MistralRsError::mcp_client(
                "config",
                format!("MCP configuration file not found: {}", path.display()),
            ));
        }

        let content = std::fs::read_to_string(path).map_err(|e| {
            MistralRsError::mcp_client(
                "config",
                format!("Failed to read MCP config file '{}': {}", path.display(), e),
            )
        })?;

        serde_json::from_str(&content).map_err(|e| {
            MistralRsError::mcp_client(
                "config",
                format!("Failed to parse MCP config file '{}': {}", path.display(), e),
            )
        })
    }

    /// Validate the MCP configuration.
    ///
    /// Checks for common issues like empty server lists, invalid URLs, etc.
    pub fn validate(&self) -> Result<()> {
        if self.servers.is_empty() {
            return Err(MistralRsError::mcp_client(
                "config",
                "MCP configuration has no servers defined",
            ));
        }

        let mut seen_ids = std::collections::HashSet::new();
        for server in &self.servers {
            // Check for duplicate IDs
            if !seen_ids.insert(&server.id) {
                return Err(MistralRsError::mcp_client(
                    &server.id,
                    format!("Duplicate server ID: '{}'", server.id),
                ));
            }

            // Validate server name
            if server.name.is_empty() {
                return Err(MistralRsError::mcp_client(
                    &server.id,
                    format!("Server '{}' has empty name", server.id),
                ));
            }

            // Validate source-specific requirements
            match &server.source {
                McpServerSource::Http { url, .. } | McpServerSource::WebSocket { url, .. } => {
                    if url.is_empty() {
                        return Err(MistralRsError::mcp_client(
                            &server.id,
                            format!("Server '{}' has empty URL", server.id),
                        ));
                    }
                    if !url.starts_with("http://")
                        && !url.starts_with("https://")
                        && !url.starts_with("ws://")
                        && !url.starts_with("wss://")
                    {
                        return Err(MistralRsError::mcp_client(
                            &server.id,
                            format!("Server '{}' has invalid URL scheme: {}", server.id, url),
                        ));
                    }
                }
                McpServerSource::Process { command, .. } => {
                    if command.is_empty() {
                        return Err(MistralRsError::mcp_client(
                            &server.id,
                            format!("Server '{}' has empty command", server.id),
                        ));
                    }
                }
            }
        }

        Ok(())
    }

    /// Get the number of enabled servers.
    pub fn enabled_server_count(&self) -> usize {
        self.servers.iter().filter(|s| s.enabled).count()
    }
}

impl McpServerConfig {
    /// Create a new HTTP-based MCP server configuration.
    pub fn http(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            source: McpServerSource::Http { url: url.into(), timeout_secs: None, headers: None },
            ..Default::default()
        }
    }

    /// Create a new process-based MCP server configuration.
    pub fn process(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            source: McpServerSource::Process {
                command: command.into(),
                args: Vec::new(),
                work_dir: None,
                env: None,
            },
            ..Default::default()
        }
    }

    /// Create a new WebSocket-based MCP server configuration.
    pub fn websocket(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            source: McpServerSource::WebSocket {
                url: url.into(),
                timeout_secs: None,
                headers: None,
            },
            ..Default::default()
        }
    }

    /// Set the server ID.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();        self
    }

    /// Set the tool prefix.
    pub fn with_tool_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.tool_prefix = Some(prefix.into());        self
    }

    /// Set the bearer token for authentication.
    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.bearer_token = Some(token.into());        self
    }

    /// Disable this server.
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// Add command arguments (for process-based servers).
    pub fn with_args(mut self, args: Vec<String>) -> Self {
        if let McpServerSource::Process { args: ref mut existing_args, .. } = self.source {
            *existing_args = args;
        }
        self
    }

    /// Set the working directory (for process-based servers).
    pub fn with_work_dir(mut self, work_dir: impl Into<String>) -> Self {
        if let McpServerSource::Process { work_dir: ref mut wd, .. } = self.source {
            *wd = Some(work_dir.into());
        }
        self
    }

    /// Set environment variables (for process-based servers).
    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        if let McpServerSource::Process { env: ref mut e, .. } = self.source {
            *e = Some(env);
        }
        self
    }

    /// Set the timeout in seconds (for HTTP/WebSocket servers).
    pub fn with_timeout(mut self, timeout_secs: u64) -> Self {
        match &mut self.source {
            McpServerSource::Http { timeout_secs: t, .. }
            | McpServerSource::WebSocket { timeout_secs: t, .. } => {
                *t = Some(timeout_secs);
            }
            _ => {}
        }
        self
    }

    /// Set custom headers (for HTTP/WebSocket servers).
    pub fn with_headers(mut self, headers: HashMap<String, String>) -> Self {
        match &mut self.source {
            McpServerSource::Http { headers: h, .. }
            | McpServerSource::WebSocket { headers: h, .. } => {
                *h = Some(headers);
            }
            _ => {}
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_client_config_default() {
        let config = McpClientConfig::default();
        assert!(config.servers.is_empty());
        assert!(config.auto_register_tools);
        assert!(config.tool_timeout_secs.is_none());
        assert_eq!(config.max_concurrent_calls, Some(1));
    }

    #[test]
    fn test_mcp_server_config_http() {
        let server = McpServerConfig::http("Test Server", "https://api.example.com/mcp");
        assert_eq!(server.name, "Test Server");
        assert!(server.enabled);
        assert!(matches!(server.source, McpServerSource::Http { .. }));
    }

    #[test]
    fn test_mcp_server_config_process() {
        let server = McpServerConfig::process("Filesystem", "mcp-server-filesystem")
            .with_args(vec!["--root".to_string(), "/tmp".to_string()])
            .with_tool_prefix("fs");

        assert_eq!(server.name, "Filesystem");
        assert_eq!(server.tool_prefix, Some("fs".to_string()));
        if let McpServerSource::Process { command, args, .. } = &server.source {
            assert_eq!(command, "mcp-server-filesystem");
            assert_eq!(args.len(), 2);
        } else {
            panic!("Expected Process source");
        }
    }

    #[test]
    fn test_mcp_server_config_websocket() {
        let server = McpServerConfig::websocket("Realtime", "wss://realtime.example.com/mcp")
            .with_bearer_token("secret-token")
            .with_timeout(30);

        assert_eq!(server.name, "Realtime");
        assert_eq!(server.bearer_token, Some("secret-token".to_string()));
        if let McpServerSource::WebSocket { url, timeout_secs, .. } = &server.source {
            assert_eq!(url, "wss://realtime.example.com/mcp");
            assert_eq!(*timeout_secs, Some(30));
        } else {
            panic!("Expected WebSocket source");
        }
    }

    #[test]
    fn test_mcp_client_config_builder() {
        let config = McpClientConfig::new()
            .add_server(McpServerConfig::http("Server1", "https://api1.example.com"))
            .add_server(McpServerConfig::http("Server2", "https://api2.example.com"))
            .with_tool_timeout(60)
            .with_max_concurrent_calls(5);

        assert_eq!(config.servers.len(), 2);
        assert_eq!(config.tool_timeout_secs, Some(60));
        assert_eq!(config.max_concurrent_calls, Some(5));
    }

    #[test]
    fn test_mcp_client_config_validation() {
        // Empty servers should fail
        let config = McpClientConfig::new();
        assert!(config.validate().is_err());

        // Valid config should pass
        let config =
            McpClientConfig::with_server(McpServerConfig::http("Test", "https://api.example.com"));
        assert!(config.validate().is_ok());

        // Empty URL should fail
        let config = McpClientConfig::with_server(McpServerConfig::http("Test", ""));
        assert!(config.validate().is_err());

        // Invalid URL scheme should fail
        let config =
            McpClientConfig::with_server(McpServerConfig::http("Test", "ftp://invalid.com"));
        assert!(config.validate().is_err());

        // Empty command should fail
        let config = McpClientConfig::with_server(McpServerConfig::process("Test", ""));
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_mcp_client_config_serialize() {
        let config = McpClientConfig::with_server(
            McpServerConfig::http("Test", "https://api.example.com")
                .with_id("test-server")
                .with_bearer_token("token123"),
        );

        let json = serde_json::to_string(&config).unwrap();
        let parsed: McpClientConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.servers.len(), 1);
        assert_eq!(parsed.servers[0].id, "test-server");
        assert_eq!(parsed.servers[0].bearer_token, Some("token123".to_string()));
    }

    #[test]
    fn test_enabled_server_count() {
        let config = McpClientConfig::new()
            .add_server(McpServerConfig::http("Server1", "https://api1.example.com"))
            .add_server(McpServerConfig::http("Server2", "https://api2.example.com").disabled())
            .add_server(McpServerConfig::http("Server3", "https://api3.example.com"));

        assert_eq!(config.enabled_server_count(), 2);
    }
}
