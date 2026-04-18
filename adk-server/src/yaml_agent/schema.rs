//! YAML agent definition schema types.
//!
//! Defines the internal representation of YAML agent definition files,
//! including model configuration, tool references, and sub-agent references.
//! Unknown fields are preserved in a metadata map for forward compatibility.
//!
//! # Example
//!
//! ```yaml
//! name: my_agent
//! description: "A helpful assistant"
//! model:
//!   provider: gemini
//!   model_id: gemini-2.0-flash
//! instructions: |
//!   You are a helpful assistant.
//! tools:
//!   - name: get_weather
//!   - mcp:
//!       endpoint: "npx @modelcontextprotocol/server-filesystem"
//!       args: ["/data"]
//! sub_agents:
//!   - ref: researcher
//! config:
//!   temperature: 0.7
//! ```

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Top-level YAML agent definition.
///
/// Represents a complete agent configuration loaded from a YAML file.
/// Unknown fields are captured in the `metadata` map for forward compatibility.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct YamlAgentDefinition {
    /// Agent name (required).
    pub name: String,

    /// Optional human-readable description.
    #[serde(default)]
    pub description: Option<String>,

    /// Model configuration specifying provider and model ID.
    pub model: ModelConfig,

    /// Optional system instructions for the agent.
    #[serde(default)]
    pub instructions: Option<String>,

    /// Tool references — either named tools or MCP server endpoints.
    #[serde(default)]
    pub tools: Vec<ToolReference>,

    /// Sub-agent references resolved from other loaded YAML files.
    #[serde(default)]
    pub sub_agents: Vec<SubAgentReference>,

    /// Arbitrary configuration parameters (e.g., temperature, max_tokens).
    #[serde(default)]
    pub config: HashMap<String, serde_json::Value>,

    /// Forward-compatibility: unknown fields preserved here.
    #[serde(flatten)]
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Model configuration for an agent.
///
/// Specifies which LLM provider and model to use, along with optional
/// generation parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ModelConfig {
    /// LLM provider name (e.g., "gemini", "openai", "anthropic").
    pub provider: String,

    /// Model identifier within the provider (e.g., "gemini-2.0-flash").
    pub model_id: String,

    /// Optional sampling temperature.
    #[serde(default)]
    pub temperature: Option<f64>,

    /// Optional maximum number of tokens to generate.
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

/// A reference to a tool, either by name or as an MCP server endpoint.
///
/// Uses serde's untagged enum representation so that YAML entries like
/// `- name: get_weather` and `- mcp: { endpoint: "..." }` are both accepted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ToolReference {
    /// A tool resolved by name from the registered toolset.
    Named {
        /// Tool name to resolve.
        name: String,
    },
    /// An MCP server tool reference.
    Mcp {
        /// MCP server configuration.
        mcp: McpToolReference,
    },
}

/// MCP server tool reference specifying an endpoint and optional arguments.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpToolReference {
    /// MCP server endpoint command or URL.
    pub endpoint: String,

    /// Optional command-line arguments for the MCP server.
    #[serde(default)]
    pub args: Vec<String>,
}

/// A reference to a sub-agent by name.
///
/// The referenced agent is resolved from other loaded YAML definitions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubAgentReference {
    /// Name of the referenced sub-agent.
    #[serde(rename = "ref")]
    pub reference: String,
}
