use crate::types::Content;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub invocation_id: String,
    pub branch: String,
    pub author: String,
    pub content: Option<Content>,
    pub actions: EventActions,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventActions {
    pub state_delta: HashMap<String, serde_json::Value>,
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
            content: None,
            actions: EventActions::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_creation() {
        let event = Event::new("inv-123");
        assert_eq!(event.invocation_id, "inv-123");
        assert!(!event.id.is_empty());
    }

    #[test]
    fn test_event_actions_default() {
        let actions = EventActions::default();
        assert!(actions.state_delta.is_empty());
        assert!(!actions.skip_summarization);
    }
}
