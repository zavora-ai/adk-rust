use crate::model::LlmResponse;
use crate::types::Content;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// State scope prefixes
pub const KEY_PREFIX_APP: &str = "app:";
pub const KEY_PREFIX_TEMP: &str = "temp:";
pub const KEY_PREFIX_USER: &str = "user:";

/// Event represents a single interaction in a conversation.
/// This struct embeds LlmResponse to match ADK-Go's design pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub invocation_id: String,
    pub branch: String,
    pub author: String,
    /// The LLM response containing content and metadata.
    /// Access content via `event.llm_response.content`.
    #[serde(flatten)]
    pub llm_response: LlmResponse,
    pub actions: EventActions,
    /// IDs of long-running tools associated with this event.
    #[serde(default)]
    pub long_running_tool_ids: Vec<String>,
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
            llm_response: LlmResponse::default(),
            actions: EventActions::default(),
            long_running_tool_ids: Vec::new(),
        }
    }

    /// Convenience method to access content directly.
    pub fn content(&self) -> Option<&Content> {
        self.llm_response.content.as_ref()
    }

    /// Convenience method to set content directly.
    pub fn set_content(&mut self, content: Content) {
        self.llm_response.content = Some(content);
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

    #[test]
    fn test_state_prefixes() {
        assert_eq!(KEY_PREFIX_APP, "app:");
        assert_eq!(KEY_PREFIX_TEMP, "temp:");
        assert_eq!(KEY_PREFIX_USER, "user:");
    }
}
