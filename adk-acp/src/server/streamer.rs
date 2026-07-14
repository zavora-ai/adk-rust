//! Maps ADK-Rust events to official ACP v1 session updates.

use adk_core::{Content, Event, Part};
use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, SessionUpdate, TextContent, ToolCall, ToolCallStatus,
    ToolCallUpdate, ToolCallUpdateFields,
};

/// Converts the typed event stream produced by the ADK-Rust Runner into ACP
/// `session/update` payloads. The transport sends each returned update as soon
/// as the corresponding Runner event arrives.
pub struct ResponseStreamer;

impl ResponseStreamer {
    /// Convert one ADK event into zero or more ACP updates while preserving the
    /// order of content parts inside the event.
    pub fn map_event(event: &Event) -> Vec<SessionUpdate> {
        let mut updates = Vec::new();
        if let Some(content) = event.content() {
            Self::map_content(content, &mut updates);
        }
        updates
    }

    fn map_content(content: &Content, updates: &mut Vec<SessionUpdate>) {
        for part in &content.parts {
            match part {
                Part::Text { text } if !text.is_empty() => {
                    updates.push(SessionUpdate::AgentMessageChunk(ContentChunk::new(
                        ContentBlock::Text(TextContent::new(text.clone())),
                    )));
                }
                Part::Thinking { thinking, .. } if !thinking.is_empty() => {
                    updates.push(SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                        ContentBlock::Text(TextContent::new(thinking.clone())),
                    )));
                }
                Part::FunctionCall { name, args, id, .. } => {
                    let call_id = id.clone().unwrap_or_else(|| format!("{name}-call"));
                    updates.push(SessionUpdate::ToolCall(
                        ToolCall::new(call_id, name.clone())
                            .status(ToolCallStatus::InProgress)
                            .raw_input(args.clone()),
                    ));
                }
                Part::FunctionResponse { function_response, id } => {
                    let call_id =
                        id.clone().unwrap_or_else(|| format!("{}-call", function_response.name));
                    updates.push(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                        call_id,
                        ToolCallUpdateFields::new()
                            .status(ToolCallStatus::Completed)
                            .raw_output(function_response.response.clone()),
                    )));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_text_thought_and_tool_lifecycle_in_order() {
        let mut event = Event::new("inv-1");
        let mut content = Content::new("model");
        content
            .parts
            .push(Part::Thinking { thinking: "Inspect the project".into(), signature: None });
        content.parts.push(Part::Text { text: "I will inspect it.".into() });
        content.parts.push(Part::FunctionCall {
            name: "read_file".into(),
            args: serde_json::json!({"path":"src/main.rs"}),
            id: Some("call-1".into()),
            thought_signature: None,
        });
        event.set_content(content);

        let updates = ResponseStreamer::map_event(&event);
        assert_eq!(updates.len(), 3);
        assert!(matches!(updates[0], SessionUpdate::AgentThoughtChunk(_)));
        assert!(matches!(updates[1], SessionUpdate::AgentMessageChunk(_)));
        assert!(matches!(updates[2], SessionUpdate::ToolCall(_)));
    }

    #[test]
    fn maps_function_response_to_completed_tool_update() {
        let mut event = Event::new("inv-2");
        let mut content = Content::new("function");
        content.parts.push(Part::FunctionResponse {
            function_response: adk_core::FunctionResponseData::new(
                "read_file",
                serde_json::json!({"content":"fn main() {}"}),
            ),
            id: Some("call-1".into()),
        });
        event.set_content(content);

        let updates = ResponseStreamer::map_event(&event);
        assert!(matches!(updates.as_slice(), [SessionUpdate::ToolCallUpdate(_)]));
    }
}
