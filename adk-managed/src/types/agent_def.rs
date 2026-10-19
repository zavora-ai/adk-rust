//! Declarative agent definition type.
//!
//! `ManagedAgentDef` is the top-level struct that describes an agent declaratively.
//! It serializes to the agent definition a managed agent service accepts and is the input
//! to `ManagedAgentRuntime::create()`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{McpServerConfig, ModelRef, PermissionPolicy, SkillRef, ToolConfig};

/// Declarative agent definition: model, system prompt, tools, MCP servers, skills, and
/// permission policy, in the service's snake_case wire shape.
///
/// This struct fully describes an agent's configuration: which model to use,
/// system prompt, available tools, MCP servers, skills, and permission policies.
/// The runtime builds a runnable agent from this definition.
///
/// # Examples
///
/// ```rust
/// use adk_managed::types::ManagedAgentDef;
///
/// // Deserialize from JSON (recommended for external callers)
/// let json = serde_json::json!({
///     "name": "my-assistant",
///     "model": "gemini-3.7-flash",
///     "system": "You are a helpful assistant."
/// });
/// let def: ManagedAgentDef = serde_json::from_value(json).unwrap();
/// assert_eq!(def.name, "my-assistant");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ManagedAgentDef {
    /// Human-readable agent name.
    pub name: String,
    /// Provider-neutral model reference.
    pub model: ModelRef,
    /// System prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Agent description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Tool declarations (built-in + custom).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolConfig>,
    /// MCP server configurations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<McpServerConfig>,
    /// Skill references.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<SkillRef>,
    /// Permission policy for tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_policy: Option<PermissionPolicy>,
    /// Caller metadata (arbitrary key-value pairs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, String>>,
}

impl ManagedAgentDef {
    /// Create a new `ManagedAgentDef` with required fields and defaults for optional ones.
    ///
    /// # Arguments
    ///
    /// * `name` - Human-readable agent name
    /// * `model` - Provider-neutral model reference
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_managed::types::{ManagedAgentDef, ModelRef};
    ///
    /// let def = ManagedAgentDef::new("my-agent", ModelRef::Shorthand("gemini-3.7-flash".to_string()))
    ///     .with_system("You are a helpful assistant.");
    /// ```
    pub fn new(name: impl Into<String>, model: ModelRef) -> Self {
        Self {
            name: name.into(),
            model,
            system: None,
            description: None,
            tools: Vec::new(),
            mcp_servers: Vec::new(),
            skills: Vec::new(),
            permission_policy: None,
            metadata: None,
        }
    }

    /// Set the system prompt.
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Set the agent description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the tool declarations.
    pub fn with_tools(mut self, tools: Vec<ToolConfig>) -> Self {
        self.tools = tools;
        self
    }

    /// Set the MCP server configurations.
    pub fn with_mcp_servers(mut self, mcp_servers: Vec<McpServerConfig>) -> Self {
        self.mcp_servers = mcp_servers;
        self
    }

    /// Set the skill references.
    pub fn with_skills(mut self, skills: Vec<SkillRef>) -> Self {
        self.skills = skills;
        self
    }

    /// Set the permission policy.
    pub fn with_permission_policy(mut self, policy: PermissionPolicy) -> Self {
        self.permission_policy = Some(policy);
        self
    }

    /// Set caller metadata.
    pub fn with_metadata(mut self, metadata: BTreeMap<String, String>) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::types::{ModelConfig, PermissionMode, Provider};

    #[test]
    fn test_serialize_full_def_matches_wire_shape() {
        let def = ManagedAgentDef {
            name: "research-agent".to_string(),
            model: ModelRef::Structured {
                provider: Provider::Openai,
                model: ModelConfig::Name("gpt-4.1".to_string()),
                speed: None,
            },
            system: Some("You are a research assistant.".to_string()),
            description: Some("Researches topics using web search and custom tools".to_string()),
            tools: vec![
                ToolConfig::WebSearch {},
                ToolConfig::Custom {
                    name: "get_papers".to_string(),
                    description: Some("Search academic papers".to_string()),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "query": {"type": "string"},
                            "limit": {"type": "integer"}
                        },
                        "required": ["query"]
                    }),
                },
            ],
            mcp_servers: vec![McpServerConfig {
                name: "arxiv-server".to_string(),
                transport: "stdio".to_string(),
                command: Some("npx".to_string()),
                args: vec!["arxiv-mcp-server".to_string()],
                url: None,
                env: HashMap::new(),
                auto_approve: vec!["search".to_string()],
            }],
            skills: vec![SkillRef { skill_id: "web-research".to_string() }],
            permission_policy: Some(PermissionPolicy {
                default: PermissionMode::AutoApprove,
                tools: {
                    let mut m = HashMap::new();
                    m.insert("delete_file".to_string(), PermissionMode::Prompt);
                    m
                },
            }),
            metadata: Some({
                let mut m = BTreeMap::new();
                m.insert("team".to_string(), "platform".to_string());
                m.insert("version".to_string(), "1.0".to_string());
                m
            }),
        };

        let json = serde_json::to_value(&def).unwrap();

        // Verify top-level fields
        assert_eq!(json["name"], "research-agent");
        assert_eq!(json["system"], "You are a research assistant.");
        assert_eq!(json["description"], "Researches topics using web search and custom tools");

        // Verify model (structured form)
        let model = &json["model"];
        assert_eq!(model["provider"], "openai");
        assert_eq!(model["model"], "gpt-4.1");
        assert!(model.get("speed").is_none());

        // Verify tools array
        let tools = json["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["type"], "web_search");
        assert_eq!(tools[1]["type"], "custom");
        assert_eq!(tools[1]["name"], "get_papers");
        assert!(tools[1]["input_schema"]["properties"]["query"].is_object());

        // Verify mcp_servers
        let mcp = json["mcp_servers"].as_array().unwrap();
        assert_eq!(mcp.len(), 1);
        assert_eq!(mcp[0]["name"], "arxiv-server");
        assert_eq!(mcp[0]["transport"], "stdio");
        assert_eq!(mcp[0]["command"], "npx");
        assert_eq!(mcp[0]["args"][0], "arxiv-mcp-server");
        assert_eq!(mcp[0]["auto_approve"][0], "search");

        // Verify skills
        let skills = json["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0]["skill_id"], "web-research");

        // Verify permission_policy
        let policy = &json["permission_policy"];
        assert_eq!(policy["default"], "auto_approve");
        assert_eq!(policy["tools"]["delete_file"], "prompt");

        // Verify metadata (BTreeMap ensures sorted keys)
        let metadata = &json["metadata"];
        assert_eq!(metadata["team"], "platform");
        assert_eq!(metadata["version"], "1.0");
    }

    #[test]
    fn test_deserialize_full_def() {
        let json = serde_json::json!({
            "name": "test-agent",
            "model": "gemini-2.5-flash",
            "system": "Be helpful.",
            "tools": [
                {"type": "bash"},
                {"type": "filesystem"}
            ],
            "skills": [{"skill_id": "coding"}],
            "metadata": {"env": "staging"}
        });

        let def: ManagedAgentDef = serde_json::from_value(json).unwrap();
        assert_eq!(def.name, "test-agent");
        assert_eq!(def.system, Some("Be helpful.".to_string()));
        assert_eq!(def.tools.len(), 2);
        assert_eq!(def.skills.len(), 1);
        assert_eq!(def.mcp_servers.len(), 0);
        assert_eq!(def.permission_policy, None);
        assert_eq!(def.description, None);
        assert_eq!(def.metadata.as_ref().unwrap().get("env"), Some(&"staging".to_string()));
    }

    #[test]
    fn test_minimal_def_omits_optional_fields() {
        let def = ManagedAgentDef {
            name: "minimal".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let json = serde_json::to_value(&def).unwrap();
        let obj = json.as_object().unwrap();

        // Only required fields present
        assert!(obj.contains_key("name"));
        assert!(obj.contains_key("model"));

        // Optional fields omitted via skip_serializing_if
        assert!(!obj.contains_key("system"));
        assert!(!obj.contains_key("description"));
        assert!(!obj.contains_key("tools"));
        assert!(!obj.contains_key("mcp_servers"));
        assert!(!obj.contains_key("skills"));
        assert!(!obj.contains_key("permission_policy"));
        assert!(!obj.contains_key("metadata"));
    }

    #[test]
    fn test_round_trip_serialization() {
        let def = ManagedAgentDef {
            name: "roundtrip-agent".to_string(),
            model: ModelRef::Shorthand("claude-3.5-sonnet".to_string()),
            system: Some("System prompt".to_string()),
            description: None,
            tools: vec![ToolConfig::Bash {}],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let json_str = serde_json::to_string(&def).unwrap();
        let deserialized: ManagedAgentDef = serde_json::from_str(&json_str).unwrap();

        assert_eq!(deserialized.name, "roundtrip-agent");
        assert_eq!(deserialized.system, Some("System prompt".to_string()));
        assert_eq!(deserialized.tools.len(), 1);
    }
}
