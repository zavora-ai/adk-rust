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
pub struct Edge {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub condition: Option<String>,
}

impl Edge {
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self { from: from.into(), to: to.into(), condition: None }
    }

    pub fn conditional(
        from: impl Into<String>,
        to: impl Into<String>,
        condition: impl Into<String>,
    ) -> Self {
        Self { from: from.into(), to: to.into(), condition: Some(condition.into()) }
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
