//! MCP server configuration types.
//!
//! Defines [`McpServerConfig`], [`RestartPolicy`], and the internal [`McpJsonFile`]
//! for parsing Kiro's `mcp.json` format.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for a single managed MCP server.
///
/// Deserialized from Kiro's `mcp.json` format with camelCase field names.
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::manager::McpServerConfig;
///
/// let json = r#"{
///     "command": "npx",
///     "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
///     "env": {},
///     "disabled": false,
///     "autoApprove": ["read_file"]
/// }"#;
///
/// let config: McpServerConfig = serde_json::from_str(json).unwrap();
/// assert_eq!(config.command, "npx");
/// assert!(!config.disabled);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    /// Executable command to spawn the MCP server process.
    pub command: String,

    /// Command-line arguments passed to the server process.
    #[serde(default)]
    pub args: Vec<String>,

    /// Environment variables set for the server process.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// When `true`, the manager skips starting this server and sets its status to `Disabled`.
    #[serde(default)]
    pub disabled: bool,

    /// Tool names pre-approved for execution without confirmation.
    #[serde(default)]
    pub auto_approve: Vec<String>,

    /// Optional restart policy controlling auto-restart behavior with exponential backoff.
    #[serde(default)]
    pub restart_policy: Option<RestartPolicy>,
}

/// Controls auto-restart behavior with exponential backoff.
///
/// When a managed server crashes, the manager uses this policy to determine
/// how long to wait before restarting and when to give up.
///
/// # Backoff Formula
///
/// ```text
/// delay(attempt) = min(initial_delay_ms × backoff_multiplier ^ attempt, max_delay_ms)
/// ```
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::manager::RestartPolicy;
///
/// let json = r#"{
///     "initialDelayMs": 500,
///     "maxDelayMs": 15000,
///     "backoffMultiplier": 2.0,
///     "maxRestartAttempts": 5
/// }"#;
///
/// let policy: RestartPolicy = serde_json::from_str(json).unwrap();
/// assert_eq!(policy.initial_delay_ms, 500);
/// assert_eq!(policy.max_restart_attempts, 5);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RestartPolicy {
    /// Initial delay in milliseconds before the first restart attempt. Default: 1000.
    #[serde(default = "default_initial_delay")]
    pub initial_delay_ms: u64,

    /// Maximum delay in milliseconds between restart attempts. Default: 30000.
    #[serde(default = "default_max_delay")]
    pub max_delay_ms: u64,

    /// Multiplier applied to the delay after each failed attempt. Default: 2.0.
    #[serde(default = "default_backoff_multiplier")]
    pub backoff_multiplier: f64,

    /// Maximum number of consecutive restart attempts before giving up. Default: 10.
    #[serde(default = "default_max_restart_attempts")]
    pub max_restart_attempts: u32,
}

fn default_initial_delay() -> u64 {
    1000
}

fn default_max_delay() -> u64 {
    30000
}

fn default_backoff_multiplier() -> f64 {
    2.0
}

fn default_max_restart_attempts() -> u32 {
    10
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            initial_delay_ms: default_initial_delay(),
            max_delay_ms: default_max_delay(),
            backoff_multiplier: default_backoff_multiplier(),
            max_restart_attempts: default_max_restart_attempts(),
        }
    }
}

/// Internal representation of Kiro's `mcp.json` file format.
///
/// The top-level JSON object contains a `mcpServers` key mapping server IDs
/// to their configurations.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // Used by McpServerManager in manager.rs (Task 3+)
pub(crate) struct McpJsonFile {
    /// Map of server ID to server configuration.
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_restart_policy() {
        let policy = RestartPolicy::default();
        assert_eq!(policy.initial_delay_ms, 1000);
        assert_eq!(policy.max_delay_ms, 30000);
        assert!((policy.backoff_multiplier - 2.0).abs() < f64::EPSILON);
        assert_eq!(policy.max_restart_attempts, 10);
    }

    #[test]
    fn test_mcp_server_config_defaults() {
        let json = r#"{"command": "echo"}"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.command, "echo");
        assert!(config.args.is_empty());
        assert!(config.env.is_empty());
        assert!(!config.disabled);
        assert!(config.auto_approve.is_empty());
        assert!(config.restart_policy.is_none());
    }

    #[test]
    fn test_mcp_json_file_parsing() {
        let json = r#"{
            "mcpServers": {
                "filesystem": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
                    "env": {},
                    "disabled": false,
                    "autoApprove": ["read_file", "list_directory"]
                }
            }
        }"#;
        let file: McpJsonFile = serde_json::from_str(json).unwrap();
        assert_eq!(file.mcp_servers.len(), 1);
        let fs_config = &file.mcp_servers["filesystem"];
        assert_eq!(fs_config.command, "npx");
        assert_eq!(fs_config.auto_approve, vec!["read_file", "list_directory"]);
    }

    #[test]
    fn test_restart_policy_serde_defaults() {
        let json = r#"{}"#;
        let policy: RestartPolicy = serde_json::from_str(json).unwrap();
        assert_eq!(policy.initial_delay_ms, 1000);
        assert_eq!(policy.max_delay_ms, 30000);
        assert!((policy.backoff_multiplier - 2.0).abs() < f64::EPSILON);
        assert_eq!(policy.max_restart_attempts, 10);
    }

    #[test]
    fn test_config_round_trip() {
        let config = McpServerConfig {
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "server".to_string()],
            env: HashMap::from([("KEY".to_string(), "value".to_string())]),
            disabled: true,
            auto_approve: vec!["tool1".to_string()],
            restart_policy: Some(RestartPolicy {
                initial_delay_ms: 500,
                max_delay_ms: 10000,
                backoff_multiplier: 1.5,
                max_restart_attempts: 3,
            }),
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: McpServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, deserialized);
    }
}
