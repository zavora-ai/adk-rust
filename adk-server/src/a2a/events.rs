use crate::a2a::{metadata::to_a2a_meta_key, parts, Message, Role};
use adk_core::{Content, Event, EventActions, Result};
use serde_json::{Map, Value};

pub fn event_to_message(event: &Event) -> Result<Message> {
    let role = if event.author == "user" {
        Role::User
    } else {
        Role::Agent
    };

    let content = event.llm_response.content.as_ref().ok_or_else(|| {
        adk_core::AdkError::Agent("Event has no content".to_string())
    })?;

    let a2a_parts = parts::adk_parts_to_a2a(&content.parts, &[])?;

    let mut metadata = Map::new();
    if event.actions.escalate {
        metadata.insert(to_a2a_meta_key("escalate"), Value::Bool(true));
    }
    if let Some(agent) = &event.actions.transfer_to_agent {
        metadata.insert(
            to_a2a_meta_key("transfer_to_agent"),
            Value::String(agent.clone()),
        );
    }

    Ok(Message::builder()
        .role(role)
        .parts(a2a_parts)
        .message_id(event.invocation_id.clone())
        .metadata(if metadata.is_empty() { None } else { Some(metadata) })
        .build())
}

pub fn message_to_event(message: &Message, invocation_id: String) -> Result<Event> {
    let adk_parts = parts::a2a_parts_to_adk(&message.parts)?;
    
    let mut actions = EventActions::default();
    if let Some(meta) = &message.metadata {
        if let Some(Value::Bool(true)) = meta.get(&to_a2a_meta_key("escalate")) {
            actions.escalate = true;
        }
        if let Some(Value::String(agent)) = meta.get(&to_a2a_meta_key("transfer_to_agent")) {
            actions.transfer_to_agent = Some(agent.clone());
        }
    }

    let author = match message.role {
        Role::User => "user".to_string(),
        Role::Agent => "agent".to_string(),
    };

    let mut event = Event::new(invocation_id);
    event.author = author;
    event.llm_response.content = Some(Content {
        role: "user".to_string(),
        parts: adk_parts,
    });
    event.actions = actions;
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_to_event() {
        let message = Message::builder()
            .role(Role::User)
            .parts(vec![crate::a2a::Part::text("Hello".to_string())])
            .message_id("msg-123".to_string())
            .build();

        let event = message_to_event(&message, "inv-123".to_string()).unwrap();
        assert_eq!(event.invocation_id, "inv-123");
        assert_eq!(event.author, "user");
    }
}
