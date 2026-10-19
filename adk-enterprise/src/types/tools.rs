//! Tool configuration types.
//!
//! [`ToolConfig`], [`McpServerConfig`], [`SkillRef`], and [`PermissionPolicy`], as an agent
//! definition carries them.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Tool configuration for an agent.
///
/// Supports built-in platform tools (identified by type string) and
/// custom user-defined tools with a JSON schema.
///
/// # Wire Format
///
/// Built-in: `{"type": "builtin", "name": "bash"}`
/// Custom: `{"type": "custom", "name": "...", "description": "...", "input_schema": {...}}`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum ToolConfig {
    /// A built-in platform tool (e.g., "bash", "web_search", "code_execution").
    #[serde(rename = "builtin")]
    Builtin {
        /// The built-in tool type name.
        name: String,
    },
    /// A custom tool defined by the user.
    #[serde(rename = "custom")]
    Custom {
        /// Tool name (unique within the agent).
        name: String,
        /// Human-readable description of the tool's purpose.
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// JSON Schema for the tool's input parameters.
        #[serde(skip_serializing_if = "Option::is_none")]
        input_schema: Option<serde_json::Value>,
    },
}

impl ToolConfig {
    /// Create a built-in tool configuration.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_enterprise::types::tools::ToolConfig;
    ///
    /// let tool = ToolConfig::builtin("bash");
    /// ```
    pub fn builtin(type_name: impl Into<String>) -> Self {
        Self::Builtin { name: type_name.into() }
    }

    /// Create a custom tool configuration.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_enterprise::types::tools::ToolConfig;
    /// use serde_json::json;
    ///
    /// let tool = ToolConfig::custom(
    ///     "get_weather",
    ///     "Get weather for a city",
    ///     json!({
    ///         "type": "object",
    ///         "properties": { "city": { "type": "string" } },
    ///         "required": ["city"]
    ///     }),
    /// );
    /// ```
    pub fn custom(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: serde_json::Value,
    ) -> Self {
        Self::Custom {
            name: name.into(),
            description: Some(description.into()),
            input_schema: Some(input_schema),
        }
    }
}

/// MCP server configuration.
///
/// Defines how to connect to an MCP (Model Context Protocol) server
/// that provides additional tools to the agent.
///
/// # Wire Format
///
/// ```json
/// {
///   "name": "my-server",
///   "transport": "stdio",
///   "command": "npx",
///   "args": ["-y", "@my/mcp-server"],
///   "env": {"API_KEY": "..."},
///   "auto_approve": false
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpServerConfig {
    /// Unique name for this MCP server.
    pub name: String,
    /// Transport type: "stdio", "sse", or "http".
    pub transport: String,
    /// Command to run (for stdio transport).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Arguments for the command.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// URL for the MCP server (for sse/http transport).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Environment variables to set for the MCP server process.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    /// Whether to auto-approve tool calls from this server.
    #[serde(default)]
    pub auto_approve: bool,
}

/// Reference to an agent skill.
///
/// Skills are pre-built capability packages that can be attached to agents.
///
/// # Wire Format
///
/// ```json
/// { "skill_id": "sk_abc123" }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillRef {
    /// The skill identifier.
    pub skill_id: String,
}

/// Permission policy for tool execution.
///
/// Controls how the agent handles tool execution authorization.
///
/// # Wire Format
///
/// ```json
/// { "mode": "autoApprove" }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionPolicy {
    /// The permission mode governing tool execution.
    pub mode: PermissionMode,
}

/// Permission mode controlling tool execution.
///
/// - `AutoApprove`: Tools execute without user confirmation.
/// - `Prompt`: Tools require user confirmation before execution.
/// - `Deny`: Tool execution is blocked entirely.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    /// Tools execute without user confirmation.
    AutoApprove,
    /// Tools require user confirmation before execution.
    Prompt,
    /// Tool execution is blocked entirely.
    Deny,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ─── ToolConfig serialization tests ─────────────────────────────

    #[test]
    fn test_builtin_tool_serializes_correctly() {
        let tool = ToolConfig::builtin("bash");
        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(
            json,
            json!({
                "type": "builtin",
                "name": "bash"
            })
        );
    }

    #[test]
    fn test_custom_tool_serializes_correctly() {
        let tool = ToolConfig::custom(
            "get_weather",
            "Get weather for a city",
            json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"]
            }),
        );
        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(
            json,
            json!({
                "type": "custom",
                "name": "get_weather",
                "description": "Get weather for a city",
                "input_schema": {
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }
            })
        );
    }

    #[test]
    fn test_custom_tool_omits_none_fields() {
        let tool =
            ToolConfig::Custom { name: "my_tool".into(), description: None, input_schema: None };
        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(
            json,
            json!({
                "type": "custom",
                "name": "my_tool"
            })
        );
    }

    #[test]
    fn test_builtin_tool_round_trip() {
        let tool = ToolConfig::builtin("web_search");
        let serialized = serde_json::to_string(&tool).unwrap();
        let deserialized: ToolConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(tool, deserialized);
    }

    #[test]
    fn test_custom_tool_round_trip() {
        let tool = ToolConfig::custom("calc", "Calculate", json!({"type": "object"}));
        let serialized = serde_json::to_string(&tool).unwrap();
        let deserialized: ToolConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(tool, deserialized);
    }

    #[test]
    fn test_tool_config_deserializes_from_wire() {
        let wire = r#"{"type":"builtin","name":"code_execution"}"#;
        let tool: ToolConfig = serde_json::from_str(wire).unwrap();
        assert_eq!(tool, ToolConfig::builtin("code_execution"));
    }

    // ─── McpServerConfig serialization tests ────────────────────────

    #[test]
    fn test_mcp_server_stdio_serializes_correctly() {
        let config = McpServerConfig {
            name: "my-server".into(),
            transport: "stdio".into(),
            command: Some("npx".into()),
            args: vec!["-y".into(), "@my/mcp-server".into()],
            url: None,
            env: HashMap::from([("API_KEY".into(), "secret".into())]),
            auto_approve: false,
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(
            json,
            json!({
                "name": "my-server",
                "transport": "stdio",
                "command": "npx",
                "args": ["-y", "@my/mcp-server"],
                "env": {"API_KEY": "secret"},
                "auto_approve": false
            })
        );
    }

    #[test]
    fn test_mcp_server_sse_serializes_correctly() {
        let config = McpServerConfig {
            name: "remote-server".into(),
            transport: "sse".into(),
            command: None,
            args: vec![],
            url: Some("https://mcp.example.com/sse".into()),
            env: HashMap::new(),
            auto_approve: true,
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(
            json,
            json!({
                "name": "remote-server",
                "transport": "sse",
                "url": "https://mcp.example.com/sse",
                "auto_approve": true
            })
        );
    }

    #[test]
    fn test_mcp_server_round_trip() {
        let config = McpServerConfig {
            name: "test".into(),
            transport: "stdio".into(),
            command: Some("node".into()),
            args: vec!["server.js".into()],
            url: None,
            env: HashMap::from([("PORT".into(), "3000".into())]),
            auto_approve: false,
        };
        let serialized = serde_json::to_string(&config).unwrap();
        let deserialized: McpServerConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_mcp_server_deserializes_without_optional_fields() {
        let wire = r#"{"name":"minimal","transport":"http","auto_approve":false}"#;
        let config: McpServerConfig = serde_json::from_str(wire).unwrap();
        assert_eq!(config.name, "minimal");
        assert_eq!(config.transport, "http");
        assert_eq!(config.command, None);
        assert!(config.args.is_empty());
        assert_eq!(config.url, None);
        assert!(config.env.is_empty());
        assert!(!config.auto_approve);
    }

    // ─── SkillRef serialization tests ───────────────────────────────

    #[test]
    fn test_skill_ref_serializes_snake_case() {
        let skill = SkillRef { skill_id: "sk_abc123".into() };
        let json = serde_json::to_value(&skill).unwrap();
        assert_eq!(json, json!({"skill_id": "sk_abc123"}));
    }

    #[test]
    fn test_skill_ref_round_trip() {
        let skill = SkillRef { skill_id: "sk_test_456".into() };
        let serialized = serde_json::to_string(&skill).unwrap();
        let deserialized: SkillRef = serde_json::from_str(&serialized).unwrap();
        assert_eq!(skill, deserialized);
    }

    #[test]
    fn test_skill_ref_deserializes_from_wire() {
        let wire = r#"{"skill_id":"sk_prod_789"}"#;
        let skill: SkillRef = serde_json::from_str(wire).unwrap();
        assert_eq!(skill.skill_id, "sk_prod_789");
    }

    // ─── PermissionPolicy serialization tests ───────────────────────

    #[test]
    fn test_permission_policy_auto_approve() {
        let policy = PermissionPolicy { mode: PermissionMode::AutoApprove };
        let json = serde_json::to_value(&policy).unwrap();
        assert_eq!(json, json!({"mode": "autoApprove"}));
    }

    #[test]
    fn test_permission_policy_prompt() {
        let policy = PermissionPolicy { mode: PermissionMode::Prompt };
        let json = serde_json::to_value(&policy).unwrap();
        assert_eq!(json, json!({"mode": "prompt"}));
    }

    #[test]
    fn test_permission_policy_deny() {
        let policy = PermissionPolicy { mode: PermissionMode::Deny };
        let json = serde_json::to_value(&policy).unwrap();
        assert_eq!(json, json!({"mode": "deny"}));
    }

    #[test]
    fn test_permission_mode_round_trip() {
        for mode in [PermissionMode::AutoApprove, PermissionMode::Prompt, PermissionMode::Deny] {
            let policy = PermissionPolicy { mode: mode.clone() };
            let serialized = serde_json::to_string(&policy).unwrap();
            let deserialized: PermissionPolicy = serde_json::from_str(&serialized).unwrap();
            assert_eq!(policy, deserialized);
        }
    }

    #[test]
    fn test_permission_policy_deserializes_from_wire() {
        let wire = r#"{"mode":"autoApprove"}"#;
        let policy: PermissionPolicy = serde_json::from_str(wire).unwrap();
        assert_eq!(policy.mode, PermissionMode::AutoApprove);
    }
}
