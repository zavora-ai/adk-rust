use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Tool definition schema
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    #[serde(rename = "type")]
    pub tool_type: ToolType,
    #[serde(default)]
    pub config: Value,
    #[serde(default)]
    pub description: String,
}

impl ToolSchema {
    pub fn builtin(description: impl Into<String>) -> Self {
        Self {
            tool_type: ToolType::Builtin,
            config: Value::Object(Default::default()),
            description: description.into(),
        }
    }
}

/// Tool type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToolType {
    Builtin,
    Mcp,
    Custom,
}

/// Built-in tool identifiers
pub mod builtins {
    pub const GOOGLE_SEARCH: &str = "google_search";
    pub const WEB_BROWSE: &str = "web_browse";
    pub const CODE_EXEC: &str = "code_exec";
    pub const FILE_READ: &str = "file_read";
    pub const FILE_WRITE: &str = "file_write";
}
