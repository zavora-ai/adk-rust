use adk_core::LlmResponse;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub invocation_id: String,
    pub branch: String,
    pub author: String,
    #[serde(flatten)]
    pub llm_response: LlmResponse,
    pub actions: EventActions,
    pub long_running_tool_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EventActions {
    pub state_delta: HashMap<String, Value>,
    pub artifact_delta: HashMap<String, i64>,
    pub skip_summarization: bool,
    pub transfer_to_agent: Option<String>,
    pub escalate: bool,
}

impl Event {
    pub fn new(invocation_id: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            invocation_id: invocation_id.into(),
            branch: String::new(),
            author: String::new(),
            llm_response: LlmResponse::new(adk_core::Content::new("model")),
            actions: EventActions::default(),
            long_running_tool_ids: Vec::new(),
        }
    }
}

pub trait Events: Send + Sync {
    fn all(&self) -> Vec<Event>;
    fn len(&self) -> usize;
    fn at(&self, index: usize) -> Option<&Event>;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
