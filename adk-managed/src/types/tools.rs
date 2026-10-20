//! Tool-related types for the managed agent runtime.
//!
//! Defines [`ToolConfig`], [`McpServerConfig`], [`SkillRef`], and
//! [`PermissionPolicy`], as a [`ManagedAgentDef`](super::ManagedAgentDef) carries them.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Tool declaration: built-in or custom (client-executed).
///
/// Built-in tools execute server-side in the sandbox. Custom tools are
/// client-executed — the runtime parks the loop until the client returns
/// a result via `user.custom_tool_result`.
///
/// # Wire Shapes
///
/// ```json
/// {"type": "bash"}
/// {"type": "filesystem"}
/// {"type": "web_search"}
/// {"type": "web_fetch"}
/// {"type": "code_execution"}
/// {"type": "custom", "name": "get_weather", "input_schema": {"type": "object"}}
/// ```
///
/// # Example
///
/// ```rust
/// use adk_managed::types::ToolConfig;
/// use serde_json::json;
///
/// let tool = ToolConfig::Custom {
///     name: "get_weather".to_string(),
///     description: Some("Get current weather".to_string()),
///     input_schema: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
/// };
/// let json = serde_json::to_string(&tool).unwrap();
/// assert!(json.contains(r#""type":"custom""#));
/// assert!(json.contains(r#""name":"get_weather""#));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolConfig {
    /// Bash shell execution tool (server-side).
    Bash {},
    /// Filesystem access tool (server-side).
    Filesystem {},
    /// Web search tool (server-side).
    WebSearch {},
    /// Web fetch/scrape tool (server-side).
    WebFetch {},
    /// Code execution tool (server-side).
    CodeExecution {},
    /// Custom client-executed tool.
    Custom {
        /// Tool name (unique within the agent definition).
        name: String,
        /// Optional human-readable description.
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// JSON Schema for the tool's input parameters.
        input_schema: serde_json::Value,
    },
}

/// MCP (Model Context Protocol) server configuration.
///
/// Declares an MCP server that the runtime should connect to for additional
/// tool capabilities.
///
/// # Wire Shape
///
/// ```json
/// {
///   "name": "my-mcp-server",
///   "transport": "stdio",
///   "command": "npx",
///   "args": ["-y", "@modelcontextprotocol/server-filesystem"],
///   "env": {"HOME": "/tmp"},
///   "auto_approve": ["read_file", "list_dir"]
/// }
/// ```
///
/// # Example
///
/// ```rust
/// use adk_managed::types::McpServerConfig;
///
/// let config = McpServerConfig {
///     name: "filesystem".to_string(),
///     transport: "stdio".to_string(),
///     command: Some("npx".to_string()),
///     args: vec!["-y".to_string(), "@mcp/server-filesystem".to_string()],
///     url: None,
///     env: Default::default(),
///     auto_approve: vec!["read_file".to_string()],
/// };
/// let json = serde_json::to_string(&config).unwrap();
/// assert!(json.contains(r#""name":"filesystem""#));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct McpServerConfig {
    /// Unique name for this MCP server.
    pub name: String,
    /// Transport type: `"stdio"`, `"sse"`, `"streamable_http"`, etc.
    pub transport: String,
    /// Command to launch the server (for stdio transport).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Arguments to pass to the command.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// URL for network-based transports (SSE, HTTP).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Environment variables to set when launching the server.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    /// Tool names that are pre-approved (no confirmation needed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auto_approve: Vec<String>,
}

/// Reference to a skill package.
///
/// Skills are pre-built capability packages that the runtime can load
/// and attach to an agent.
///
/// # Wire Shape
///
/// ```json
/// {"skill_id": "code-review-v2"}
/// ```
///
/// # Example
///
/// ```rust
/// use adk_managed::types::SkillRef;
///
/// let skill = SkillRef { skill_id: "code-review-v2".to_string() };
/// let json = serde_json::to_string(&skill).unwrap();
/// assert_eq!(json, r#"{"skill_id":"code-review-v2"}"#);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRef {
    /// The skill package identifier.
    pub skill_id: String,
}

/// Permission policy for tool execution.
///
/// Controls whether tools require confirmation before execution.
/// A default mode applies to all tools unless overridden per-tool.
///
/// # Wire Shape
///
/// ```json
/// {
///   "default": "prompt",
///   "tools": {
///     "read_file": "auto_approve",
///     "delete_file": "deny"
///   }
/// }
/// ```
///
/// # Example
///
/// ```rust
/// use adk_managed::types::{PermissionPolicy, PermissionMode};
/// use std::collections::HashMap;
///
/// let policy = PermissionPolicy {
///     default: PermissionMode::Prompt,
///     tools: HashMap::from([
///         ("read_file".to_string(), PermissionMode::AutoApprove),
///         ("delete_file".to_string(), PermissionMode::Deny),
///     ]),
/// };
/// let json = serde_json::to_value(&policy).unwrap();
/// assert_eq!(json["default"], "auto_approve".replace("auto_approve", "prompt"));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionPolicy {
    /// Default permission mode for all tools not explicitly listed.
    pub default: PermissionMode,
    /// Per-tool permission overrides. Key is the tool name.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tools: HashMap<String, PermissionMode>,
}

/// Permission mode for tool execution.
///
/// Determines whether a tool call proceeds automatically, requires
/// user confirmation, or is denied outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Tool executes without confirmation.
    AutoApprove,
    /// Tool requires user confirmation before execution.
    Prompt,
    /// Tool execution is denied.
    Deny,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- ToolConfig tests ---

    #[test]
    fn test_bash_tool_serialization() {
        let tool = ToolConfig::Bash {};
        let serialized = serde_json::to_value(&tool).unwrap();
        assert_eq!(serialized, json!({"type": "bash"}));

        let deserialized: ToolConfig = serde_json::from_value(serialized).unwrap();
        assert!(matches!(deserialized, ToolConfig::Bash {}));
    }

    #[test]
    fn test_filesystem_tool_serialization() {
        let tool = ToolConfig::Filesystem {};
        let serialized = serde_json::to_value(&tool).unwrap();
        assert_eq!(serialized, json!({"type": "filesystem"}));

        let deserialized: ToolConfig = serde_json::from_value(serialized).unwrap();
        assert!(matches!(deserialized, ToolConfig::Filesystem {}));
    }

    #[test]
    fn test_web_search_tool_serialization() {
        let tool = ToolConfig::WebSearch {};
        let serialized = serde_json::to_value(&tool).unwrap();
        assert_eq!(serialized, json!({"type": "web_search"}));

        let deserialized: ToolConfig = serde_json::from_value(serialized).unwrap();
        assert!(matches!(deserialized, ToolConfig::WebSearch {}));
    }

    #[test]
    fn test_web_fetch_tool_serialization() {
        let tool = ToolConfig::WebFetch {};
        let serialized = serde_json::to_value(&tool).unwrap();
        assert_eq!(serialized, json!({"type": "web_fetch"}));

        let deserialized: ToolConfig = serde_json::from_value(serialized).unwrap();
        assert!(matches!(deserialized, ToolConfig::WebFetch {}));
    }

    #[test]
    fn test_code_execution_tool_serialization() {
        let tool = ToolConfig::CodeExecution {};
        let serialized = serde_json::to_value(&tool).unwrap();
        assert_eq!(serialized, json!({"type": "code_execution"}));

        let deserialized: ToolConfig = serde_json::from_value(serialized).unwrap();
        assert!(matches!(deserialized, ToolConfig::CodeExecution {}));
    }

    #[test]
    fn test_custom_tool_with_description_serialization() {
        let schema = json!({
            "type": "object",
            "properties": {
                "city": {"type": "string"}
            },
            "required": ["city"]
        });

        let tool = ToolConfig::Custom {
            name: "get_weather".to_string(),
            description: Some("Get the current weather for a city".to_string()),
            input_schema: schema.clone(),
        };

        let serialized = serde_json::to_value(&tool).unwrap();
        assert_eq!(serialized["type"], "custom");
        assert_eq!(serialized["name"], "get_weather");
        assert_eq!(serialized["description"], "Get the current weather for a city");
        assert_eq!(serialized["input_schema"], schema);

        let deserialized: ToolConfig = serde_json::from_value(serialized).unwrap();
        match deserialized {
            ToolConfig::Custom { name, description, input_schema } => {
                assert_eq!(name, "get_weather");
                assert_eq!(description, Some("Get the current weather for a city".to_string()));
                assert_eq!(input_schema, schema);
            }
            _ => panic!("Expected Custom variant"),
        }
    }

    #[test]
    fn test_custom_tool_without_description_serialization() {
        let schema = json!({"type": "object"});

        let tool = ToolConfig::Custom {
            name: "my_tool".to_string(),
            description: None,
            input_schema: schema.clone(),
        };

        let serialized = serde_json::to_value(&tool).unwrap();
        assert_eq!(serialized["type"], "custom");
        assert_eq!(serialized["name"], "my_tool");
        // description should be omitted when None
        assert!(serialized.get("description").is_none());
        assert_eq!(serialized["input_schema"], schema);

        let deserialized: ToolConfig = serde_json::from_value(serialized).unwrap();
        match deserialized {
            ToolConfig::Custom { name, description, .. } => {
                assert_eq!(name, "my_tool");
                assert_eq!(description, None);
            }
            _ => panic!("Expected Custom variant"),
        }
    }

    #[test]
    fn test_tool_config_vec_round_trip() {
        let tools = vec![
            ToolConfig::Bash {},
            ToolConfig::WebSearch {},
            ToolConfig::Custom {
                name: "deploy".to_string(),
                description: Some("Deploy the app".to_string()),
                input_schema: json!({"type": "object", "properties": {"env": {"type": "string"}}}),
            },
        ];

        let serialized = serde_json::to_value(&tools).unwrap();
        let deserialized: Vec<ToolConfig> = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized.len(), 3);
        assert!(matches!(deserialized[0], ToolConfig::Bash {}));
        assert!(matches!(deserialized[1], ToolConfig::WebSearch {}));
        assert!(matches!(deserialized[2], ToolConfig::Custom { .. }));
    }

    #[test]
    fn test_unknown_tool_type_rejected() {
        let json_str = r#"{"type": "unknown_tool"}"#;
        let result: Result<ToolConfig, _> = serde_json::from_str(json_str);
        assert!(result.is_err(), "Unknown tool type should be rejected");
    }

    // --- McpServerConfig tests ---

    #[test]
    fn test_mcp_server_stdio_serialization() {
        let config = McpServerConfig {
            name: "filesystem".to_string(),
            transport: "stdio".to_string(),
            command: Some("npx".to_string()),
            args: vec!["-y".to_string(), "@modelcontextprotocol/server-filesystem".to_string()],
            url: None,
            env: HashMap::from([("HOME".to_string(), "/tmp".to_string())]),
            auto_approve: vec!["read_file".to_string(), "list_dir".to_string()],
        };

        let serialized = serde_json::to_value(&config).unwrap();
        assert_eq!(serialized["name"], "filesystem");
        assert_eq!(serialized["transport"], "stdio");
        assert_eq!(serialized["command"], "npx");
        assert_eq!(serialized["args"], json!(["-y", "@modelcontextprotocol/server-filesystem"]));
        assert!(serialized.get("url").is_none());
        assert_eq!(serialized["env"]["HOME"], "/tmp");
        assert_eq!(serialized["auto_approve"], json!(["read_file", "list_dir"]));

        let deserialized: McpServerConfig = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized.name, "filesystem");
        assert_eq!(deserialized.transport, "stdio");
        assert_eq!(deserialized.command, Some("npx".to_string()));
        assert_eq!(deserialized.args.len(), 2);
        assert_eq!(deserialized.url, None);
        assert_eq!(deserialized.env.get("HOME").unwrap(), "/tmp");
        assert_eq!(deserialized.auto_approve.len(), 2);
    }

    #[test]
    fn test_mcp_server_sse_serialization() {
        let config = McpServerConfig {
            name: "remote-tools".to_string(),
            transport: "sse".to_string(),
            command: None,
            args: vec![],
            url: Some("https://mcp.example.com/sse".to_string()),
            env: HashMap::new(),
            auto_approve: vec![],
        };

        let serialized = serde_json::to_value(&config).unwrap();
        assert_eq!(serialized["name"], "remote-tools");
        assert_eq!(serialized["transport"], "sse");
        // command, args, env, auto_approve should be omitted when empty/None
        assert!(serialized.get("command").is_none());
        assert!(serialized.get("args").is_none());
        assert_eq!(serialized["url"], "https://mcp.example.com/sse");
        assert!(serialized.get("env").is_none());
        assert!(serialized.get("auto_approve").is_none());

        let deserialized: McpServerConfig = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized.name, "remote-tools");
        assert_eq!(deserialized.transport, "sse");
        assert_eq!(deserialized.command, None);
        assert!(deserialized.args.is_empty());
        assert_eq!(deserialized.url, Some("https://mcp.example.com/sse".to_string()));
        assert!(deserialized.env.is_empty());
        assert!(deserialized.auto_approve.is_empty());
    }

    #[test]
    fn test_mcp_server_from_json_string() {
        let json_str = r#"{
            "name": "my-server",
            "transport": "stdio",
            "command": "node",
            "args": ["server.js"],
            "env": {"PORT": "3000"}
        }"#;

        let config: McpServerConfig = serde_json::from_str(json_str).unwrap();
        assert_eq!(config.name, "my-server");
        assert_eq!(config.transport, "stdio");
        assert_eq!(config.command, Some("node".to_string()));
        assert_eq!(config.args, vec!["server.js"]);
        assert_eq!(config.env.get("PORT").unwrap(), "3000");
        assert!(config.auto_approve.is_empty());
    }

    // --- SkillRef tests ---

    #[test]
    fn test_skill_ref_serialization() {
        let skill = SkillRef { skill_id: "code-review-v2".to_string() };

        let serialized = serde_json::to_value(&skill).unwrap();
        assert_eq!(serialized, json!({"skill_id": "code-review-v2"}));

        let deserialized: SkillRef = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized.skill_id, "code-review-v2");
    }

    #[test]
    fn test_skill_ref_vec_round_trip() {
        let skills = vec![
            SkillRef { skill_id: "code-review-v2".to_string() },
            SkillRef { skill_id: "testing-assistant".to_string() },
        ];

        let serialized = serde_json::to_value(&skills).unwrap();
        let deserialized: Vec<SkillRef> = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized.len(), 2);
        assert_eq!(deserialized[0].skill_id, "code-review-v2");
        assert_eq!(deserialized[1].skill_id, "testing-assistant");
    }

    // --- PermissionMode tests ---

    #[test]
    fn test_permission_mode_auto_approve_serialization() {
        let mode = PermissionMode::AutoApprove;
        let serialized = serde_json::to_value(mode).unwrap();
        assert_eq!(serialized, json!("auto_approve"));

        let deserialized: PermissionMode = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized, PermissionMode::AutoApprove);
    }

    #[test]
    fn test_permission_mode_prompt_serialization() {
        let mode = PermissionMode::Prompt;
        let serialized = serde_json::to_value(mode).unwrap();
        assert_eq!(serialized, json!("prompt"));

        let deserialized: PermissionMode = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized, PermissionMode::Prompt);
    }

    #[test]
    fn test_permission_mode_deny_serialization() {
        let mode = PermissionMode::Deny;
        let serialized = serde_json::to_value(mode).unwrap();
        assert_eq!(serialized, json!("deny"));

        let deserialized: PermissionMode = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized, PermissionMode::Deny);
    }

    // --- PermissionPolicy tests ---

    #[test]
    fn test_permission_policy_with_overrides_serialization() {
        let policy = PermissionPolicy {
            default: PermissionMode::Prompt,
            tools: HashMap::from([
                ("read_file".to_string(), PermissionMode::AutoApprove),
                ("delete_file".to_string(), PermissionMode::Deny),
            ]),
        };

        let serialized = serde_json::to_value(&policy).unwrap();
        assert_eq!(serialized["default"], "prompt");
        assert_eq!(serialized["tools"]["read_file"], "auto_approve");
        assert_eq!(serialized["tools"]["delete_file"], "deny");

        let deserialized: PermissionPolicy = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized.default, PermissionMode::Prompt);
        assert_eq!(deserialized.tools.get("read_file"), Some(&PermissionMode::AutoApprove));
        assert_eq!(deserialized.tools.get("delete_file"), Some(&PermissionMode::Deny));
    }

    #[test]
    fn test_permission_policy_without_overrides_serialization() {
        let policy =
            PermissionPolicy { default: PermissionMode::AutoApprove, tools: HashMap::new() };

        let serialized = serde_json::to_value(&policy).unwrap();
        assert_eq!(serialized["default"], "auto_approve");
        // tools should be omitted when empty
        assert!(serialized.get("tools").is_none());

        let deserialized: PermissionPolicy = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized.default, PermissionMode::AutoApprove);
        assert!(deserialized.tools.is_empty());
    }

    #[test]
    fn test_permission_policy_from_json_string() {
        let json_str = r#"{
            "default": "deny",
            "tools": {
                "read_file": "auto_approve",
                "write_file": "prompt"
            }
        }"#;

        let policy: PermissionPolicy = serde_json::from_str(json_str).unwrap();
        assert_eq!(policy.default, PermissionMode::Deny);
        assert_eq!(policy.tools.len(), 2);
        assert_eq!(policy.tools.get("read_file"), Some(&PermissionMode::AutoApprove));
        assert_eq!(policy.tools.get("write_file"), Some(&PermissionMode::Prompt));
    }

    #[test]
    fn test_permission_policy_default_only_from_json() {
        let json_str = r#"{"default": "auto_approve"}"#;
        let policy: PermissionPolicy = serde_json::from_str(json_str).unwrap();
        assert_eq!(policy.default, PermissionMode::AutoApprove);
        assert!(policy.tools.is_empty());
    }

    // --- Wire shape conformance tests ---

    #[test]
    fn test_tool_config_wire_shape() {
        // Every tool variant has exactly this wire shape.
        let tools_json = json!([
            {"type": "bash"},
            {"type": "filesystem"},
            {"type": "web_search"},
            {"type": "web_fetch"},
            {"type": "code_execution"},
            {
                "type": "custom",
                "name": "get_weather",
                "description": "Get weather for a location",
                "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}
            }
        ]);

        let tools: Vec<ToolConfig> = serde_json::from_value(tools_json.clone()).unwrap();
        assert_eq!(tools.len(), 6);

        // Re-serialize and compare
        let reserialized = serde_json::to_value(&tools).unwrap();
        assert_eq!(reserialized, tools_json);
    }

    #[test]
    fn test_mcp_server_wire_shape() {
        // An MCP server declaration has exactly this wire shape.
        let mcp_json = json!({
            "name": "my-mcp-server",
            "transport": "stdio",
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-filesystem"],
            "env": {"HOME": "/tmp"},
            "auto_approve": ["read_file", "list_dir"]
        });

        let config: McpServerConfig = serde_json::from_value(mcp_json.clone()).unwrap();
        let reserialized = serde_json::to_value(&config).unwrap();
        assert_eq!(reserialized, mcp_json);
    }

    #[test]
    fn test_permission_policy_wire_shape() {
        // A permission policy has exactly this wire shape.
        let policy_json = json!({
            "default": "prompt",
            "tools": {
                "read_file": "auto_approve",
                "delete_file": "deny"
            }
        });

        let policy: PermissionPolicy = serde_json::from_value(policy_json.clone()).unwrap();
        let reserialized = serde_json::to_value(&policy).unwrap();

        // Check structural equivalence (HashMap ordering may differ)
        assert_eq!(reserialized["default"], policy_json["default"]);
        assert_eq!(reserialized["tools"]["read_file"], policy_json["tools"]["read_file"]);
        assert_eq!(reserialized["tools"]["delete_file"], policy_json["tools"]["delete_file"]);
    }

    #[test]
    fn test_debug_and_clone_impls() {
        let tool = ToolConfig::Custom {
            name: "test".to_string(),
            description: None,
            input_schema: json!({}),
        };
        let debug_str = format!("{tool:?}");
        assert!(debug_str.contains("Custom"));

        let cloned = tool.clone();
        let original_json = serde_json::to_value(&tool).unwrap();
        let cloned_json = serde_json::to_value(&cloned).unwrap();
        assert_eq!(original_json, cloned_json);

        let mode = PermissionMode::Prompt;
        let mode_clone = mode;
        assert_eq!(mode, mode_clone);
        assert_eq!(format!("{mode:?}"), "Prompt");
    }
}
