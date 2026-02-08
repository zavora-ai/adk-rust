use serde::{Deserialize, Serialize};

/// Workflow definition schema
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkflowSchema {
    #[serde(rename = "type", default)]
    pub workflow_type: WorkflowType,
    #[serde(default)]
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

/// Workflow type
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowType {
    #[default]
    Single,
    Sequential,
    Parallel,
    Graph,
}

/// Edge between nodes
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub condition: Option<String>,
    /// Source port for multi-output nodes (e.g., Switch branch output_port)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_port: Option<String>,
    /// Target port for multi-input nodes (e.g., Merge)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_port: Option<String>,
}

impl Edge {
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self { from: from.into(), to: to.into(), condition: None, from_port: None, to_port: None }
    }

    pub fn conditional(
        from: impl Into<String>,
        to: impl Into<String>,
        condition: impl Into<String>,
    ) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            condition: Some(condition.into()),
            from_port: None,
            to_port: None,
        }
    }

    pub fn with_from_port(mut self, port: impl Into<String>) -> Self {
        self.from_port = Some(port.into());
        self
    }
}

/// Condition definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Condition {
    pub id: String,
    pub expression: String,
    #[serde(default)]
    pub description: String,
}

/// Special node identifiers
pub const START: &str = "START";
pub const END: &str = "END";
