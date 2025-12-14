use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use super::{AgentSchema, ToolSchema, WorkflowSchema};

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
            workflow: WorkflowSchema::default(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// Project-level settings
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectSettings {
    #[serde(default = "default_model")]
    pub default_model: String,
    #[serde(default)]
    pub env_vars: HashMap<String, String>,
}

fn default_model() -> String {
    "gemini-2.0-flash".to_string()
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
