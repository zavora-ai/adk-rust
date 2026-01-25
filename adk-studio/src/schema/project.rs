use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use super::{AgentSchema, ToolConfig, ToolSchema, WorkflowSchema};

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
            workflow: WorkflowSchema::default(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// Project-level settings
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSettings {
    #[serde(default = "default_model")]
    pub default_model: String,
    #[serde(default)]
    pub env_vars: HashMap<String, String>,
    // Layout settings (v2.0)
    #[serde(default)]
    pub layout_mode: Option<String>,
    #[serde(default)]
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
    #[serde(default)]
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
    "gemini-2.0-flash".to_string()
}

fn default_adk_version() -> Option<String> {
    Some("0.2.2".to_string())
}

fn default_rust_edition() -> Option<String> {
    Some("2024".to_string())
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
