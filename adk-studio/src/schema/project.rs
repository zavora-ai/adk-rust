use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use super::{AgentSchema, ToolConfig, ToolSchema, WorkflowSchema};
use crate::codegen::action_nodes::ActionNodeConfig;

/// Complete project schema
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSchema {
    pub id: Uuid,
    pub version: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub settings: ProjectSettings,
    #[serde(default)]
    pub agents: HashMap<String, AgentSchema>,
    #[serde(default)]
    pub tools: HashMap<String, ToolSchema>,
    #[serde(default)]
    pub tool_configs: HashMap<String, ToolConfig>,
    /// Action nodes for non-LLM programmatic operations (v2.0)
    #[serde(default, rename = "actionNodes")]
    pub action_nodes: HashMap<String, ActionNodeConfig>,
    #[serde(default)]
    pub workflow: WorkflowSchema,
    #[serde(default)]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl ProjectSchema {
    pub fn new(name: impl Into<String>) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: Uuid::new_v4(),
            version: "1.0".to_string(),
            name: name.into(),
            description: String::new(),
            settings: ProjectSettings::default(),
            agents: HashMap::new(),
            tools: HashMap::new(),
            tool_configs: HashMap::new(),
            action_nodes: HashMap::new(),
            workflow: WorkflowSchema::default(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// Project-level settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSettings {
    #[serde(default = "default_model")]
    pub default_model: String,
    #[serde(default)]
    pub env_vars: HashMap<String, String>,
    // Layout settings (v2.0)
    #[serde(default)]
    pub layout_mode: Option<String>,
    #[serde(default = "default_layout_direction")]
    pub layout_direction: Option<String>,
    #[serde(default)]
    pub show_data_flow_overlay: Option<bool>,
    // Code generation settings
    #[serde(default = "default_adk_version")]
    pub adk_version: Option<String>,
    #[serde(default = "default_rust_edition")]
    pub rust_edition: Option<String>,
    // Default provider
    #[serde(default)]
    pub default_provider: Option<String>,
    // Build settings
    #[serde(default = "default_true")]
    pub autobuild_enabled: Option<bool>,
    #[serde(default)]
    pub autobuild_triggers: Option<AutobuildTriggers>,
    // UI preferences
    #[serde(default)]
    pub show_minimap: Option<bool>,
    #[serde(default)]
    pub show_timeline: Option<bool>,
    #[serde(default)]
    pub console_position: Option<String>,
    // Debug mode (v2.0) - controls visibility of StateInspector and Timeline
    #[serde(default)]
    pub debug_mode: Option<bool>,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            default_model: default_model(),
            env_vars: HashMap::new(),
            layout_mode: None,
            layout_direction: default_layout_direction(),
            show_data_flow_overlay: None,
            adk_version: default_adk_version(),
            rust_edition: default_rust_edition(),
            default_provider: None,
            autobuild_enabled: default_true(),
            autobuild_triggers: None,
            show_minimap: None,
            show_timeline: None,
            console_position: None,
            debug_mode: None,
        }
    }
}

/// Autobuild trigger configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AutobuildTriggers {
    #[serde(default = "default_true")]
    pub on_agent_add: Option<bool>,
    #[serde(default = "default_true")]
    pub on_agent_delete: Option<bool>,
    #[serde(default = "default_true")]
    pub on_agent_update: Option<bool>,
    #[serde(default = "default_true")]
    pub on_tool_add: Option<bool>,
    #[serde(default = "default_true")]
    pub on_tool_update: Option<bool>,
    #[serde(default = "default_true")]
    pub on_edge_add: Option<bool>,
    #[serde(default = "default_true")]
    pub on_edge_delete: Option<bool>,
}

fn default_model() -> String {
    "gemini-2.5-flash".to_string()
}

fn default_adk_version() -> Option<String> {
    Some("0.3.0".to_string())
}

fn default_rust_edition() -> Option<String> {
    Some("2024".to_string())
}

fn default_layout_direction() -> Option<String> {
    Some("LR".to_string())
}

fn default_true() -> Option<bool> {
    Some(true)
}

/// Project metadata for listing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<&ProjectSchema> for ProjectMeta {
    fn from(p: &ProjectSchema) -> Self {
        Self {
            id: p.id,
            name: p.name.clone(),
            description: p.description.clone(),
            updated_at: p.updated_at,
        }
    }
}
