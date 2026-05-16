//! Response streaming: maps ADK Events to ACP-style notifications.
//!
//! The [`ResponseStreamer`] converts ADK [`Event`] content parts into
//! ACP notification messages, preserving ordering within and across events.

use adk_core::{Content, Event, Part};
use serde::{Deserialize, Serialize};

/// An ACP-style session notification sent to the client.
///
/// Each variant represents a different type of incremental update
/// streamed during agent execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionNotification {
    /// A chunk of text from the agent's response.
    AgentMessageChunk {
        /// The text content.
        text: String,
    },
    /// A tool call initiated by the agent.
    ToolCall {
        /// The tool name.
        name: String,
        /// The tool arguments as JSON.
        args: serde_json::Value,
    },
    /// A thinking/reasoning chunk from the agent.
    AgentThoughtChunk {
        /// The thought text.
        text: String,
    },
    /// The response is complete.
    Complete,
    /// An error occurred during execution.
    Error {
        /// Machine-readable error code.
        error_code: String,
        /// Human-readable error message.
        message: String,
    },
}

/// Maps ADK Event content to ACP SessionNotification variants.
///
/// A single ADK Event may produce multiple notifications (e.g., text + tool call).
/// The output preserves the ordering of parts within the event.
///
/// # Example
///
/// ```rust,ignore
/// use adk_acp::server::ResponseStreamer;
///
/// let notifications = ResponseStreamer::map_event(&event);
/// for notif in notifications {
///     // Send to client
/// }
/// ```
pub struct ResponseStreamer;

impl ResponseStreamer {
    /// Convert an ADK Event into zero or more ACP SessionNotification messages.
    ///
    /// Maps each content part to the appropriate notification type:
    /// - `Part::Text` → `AgentMessageChunk`
    /// - `Part::FunctionCall` → `ToolCall`
    /// - `Part::Thinking` → `AgentThoughtChunk`
    /// - Other parts are skipped gracefully.
    pub fn map_event(event: &Event) -> Vec<SessionNotification> {
        let mut notifications = Vec::new();

        if let Some(content) = event.content() {
            Self::map_content(content, &mut notifications);
        }

        notifications
    }

    /// Map content parts to notifications.
    fn map_content(content: &Content, notifications: &mut Vec<SessionNotification>) {
        for part in &content.parts {
            match part {
                Part::Text { text } => {
                    if !text.is_empty() {
                        notifications
                            .push(SessionNotification::AgentMessageChunk { text: text.clone() });
                    }
                }
                Part::FunctionCall { name, args, .. } => {
                    notifications.push(SessionNotification::ToolCall {
                        name: name.clone(),
                        args: args.clone(),
                    });
                }
                Part::Thinking { thinking, .. } => {
                    if !thinking.is_empty() {
                        notifications.push(SessionNotification::AgentThoughtChunk {
                            text: thinking.clone(),
                        });
                    }
                }
                // Skip non-streamable parts (InlineData, FileData, FunctionResponse, etc.)
                _ => {}
            }
        }
    }

    /// Create a completion notification indicating the response is done.
    pub fn make_completion() -> SessionNotification {
        SessionNotification::Complete
    }

    /// Create an error notification from an error message.
    pub fn make_error(error_code: &str, message: &str) -> SessionNotification {
        SessionNotification::Error {
            error_code: error_code.to_string(),
            message: message.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Content, Event};

    #[test]
    fn test_map_text_part() {
        let mut event = Event::new("inv-1");
        event.set_content(Content::new("model").with_text("Hello world"));

        let notifications = ResponseStreamer::map_event(&event);

        assert_eq!(notifications.len(), 1);
        match &notifications[0] {
            SessionNotification::AgentMessageChunk { text } => {
                assert_eq!(text, "Hello world");
            }
            other => panic!("expected AgentMessageChunk, got {other:?}"),
        }
    }

    #[test]
    fn test_map_function_call_part() {
        let mut event = Event::new("inv-1");
        let mut content = Content::new("model");
        content.parts.push(Part::FunctionCall {
            name: "read_file".to_string(),
            args: serde_json::json!({"path": "/tmp/test.rs"}),
            id: None,
            thought_signature: None,
        });
        event.set_content(content);

        let notifications = ResponseStreamer::map_event(&event);

        assert_eq!(notifications.len(), 1);
        match &notifications[0] {
            SessionNotification::ToolCall { name, args } => {
                assert_eq!(name, "read_file");
                assert_eq!(args, &serde_json::json!({"path": "/tmp/test.rs"}));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn test_map_thinking_part() {
        let mut event = Event::new("inv-1");
        let mut content = Content::new("model");
        content.parts.push(Part::Thinking {
            thinking: "Let me think about this...".to_string(),
            signature: None,
        });
        event.set_content(content);

        let notifications = ResponseStreamer::map_event(&event);

        assert_eq!(notifications.len(), 1);
        match &notifications[0] {
            SessionNotification::AgentThoughtChunk { text } => {
                assert_eq!(text, "Let me think about this...");
            }
            other => panic!("expected AgentThoughtChunk, got {other:?}"),
        }
    }

    #[test]
    fn test_map_empty_event() {
        let event = Event::new("inv-1");
        let notifications = ResponseStreamer::map_event(&event);
        assert!(notifications.is_empty());
    }

    #[test]
    fn test_map_multiple_parts_preserves_order() {
        let mut event = Event::new("inv-1");
        let mut content = Content::new("model");
        content
            .parts
            .push(Part::Thinking { thinking: "thinking first".to_string(), signature: None });
        content.parts.push(Part::Text { text: "then text".to_string() });
        content.parts.push(Part::FunctionCall {
            name: "tool".to_string(),
            args: serde_json::json!({}),
            id: None,
            thought_signature: None,
        });
        event.set_content(content);

        let notifications = ResponseStreamer::map_event(&event);

        assert_eq!(notifications.len(), 3);
        assert!(matches!(&notifications[0], SessionNotification::AgentThoughtChunk { .. }));
        assert!(matches!(&notifications[1], SessionNotification::AgentMessageChunk { .. }));
        assert!(matches!(&notifications[2], SessionNotification::ToolCall { .. }));
    }

    #[test]
    fn test_make_completion() {
        let notif = ResponseStreamer::make_completion();
        assert!(matches!(notif, SessionNotification::Complete));
    }

    #[test]
    fn test_make_error() {
        let notif = ResponseStreamer::make_error("execution_error", "something failed");
        match notif {
            SessionNotification::Error { error_code, message } => {
                assert_eq!(error_code, "execution_error");
                assert_eq!(message, "something failed");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }
}
