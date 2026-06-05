//! Chat message container with ID-based deduplication.
//!
//! This module provides `MessagesValue`, a specialized state container for
//! maintaining conversation history without duplicate messages when tasks
//! retry or replay.

use std::collections::HashMap;

use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A chat message role indicating the sender of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    /// A user message.
    User,
    /// An assistant message.
    Assistant,
    /// A system message.
    System,
    /// A tool message.
    Tool,
}

/// A chat message with a unique identifier.
///
/// Messages are identified by their `id` field for deduplication purposes.
/// When pushed to a `MessagesValue`, a message with the same `id` as an
/// existing message will replace it (upsert semantics).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Unique identifier for deduplication.
    pub id: String,
    /// The role of the message sender.
    pub role: MessageRole,
    /// The message content.
    pub content: String,
    /// Optional metadata associated with the message.
    pub metadata: Option<Value>,
}

/// Chat message container with ID-based deduplication.
///
/// Messages with the same ID are replaced (upsert semantics).
/// The container maintains insertion order and provides O(1) dedup lookup
/// via an internal index that is rebuilt on deserialization.
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::functional::messages::{MessagesValue, ChatMessage, MessageRole};
///
/// let mut messages = MessagesValue::new();
/// messages.push(ChatMessage {
///     id: "msg_1".to_string(),
///     role: MessageRole::User,
///     content: "Hello".to_string(),
///     metadata: None,
/// });
///
/// // Pushing a message with the same ID replaces the existing one.
/// messages.push(ChatMessage {
///     id: "msg_1".to_string(),
///     role: MessageRole::User,
///     content: "Hello, updated!".to_string(),
///     metadata: None,
/// });
///
/// assert_eq!(messages.len(), 1);
/// assert_eq!(messages.iter().next().unwrap().content, "Hello, updated!");
/// ```
#[derive(Debug, Clone, Default)]
pub struct MessagesValue {
    messages: Vec<ChatMessage>,
    /// Index for O(1) dedup lookup. Maps message ID to index in `messages`.
    id_index: HashMap<String, usize>,
}

impl MessagesValue {
    /// Create an empty message collection.
    pub fn new() -> Self {
        Self { messages: Vec::new(), id_index: HashMap::new() }
    }

    /// Append a message (replaces existing if same ID — upsert semantics).
    pub fn push(&mut self, message: ChatMessage) {
        if let Some(&existing_idx) = self.id_index.get(&message.id) {
            // Replace the existing message at the same position.
            self.messages[existing_idx] = message;
        } else {
            let idx = self.messages.len();
            self.id_index.insert(message.id.clone(), idx);
            self.messages.push(message);
        }
    }

    /// Extend with multiple messages, applying upsert semantics for each.
    pub fn extend(&mut self, messages: impl IntoIterator<Item = ChatMessage>) {
        for message in messages {
            self.push(message);
        }
    }

    /// Iterate over all messages in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &ChatMessage> {
        self.messages.iter()
    }

    /// Filter messages by role.
    pub fn by_role(&self, role: MessageRole) -> Vec<&ChatMessage> {
        self.messages.iter().filter(|m| m.role == role).collect()
    }

    /// Number of messages.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Rebuild the `id_index` from the current messages Vec.
    /// Called after deserialization to restore the index.
    fn rebuild_index(&mut self) {
        self.id_index.clear();
        for (idx, msg) in self.messages.iter().enumerate() {
            self.id_index.insert(msg.id.clone(), idx);
        }
    }
}

impl Serialize for MessagesValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("MessagesValue", 1)?;
        state.serialize_field("messages", &self.messages)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for MessagesValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "lowercase")]
        enum Field {
            Messages,
        }

        struct MessagesValueVisitor;

        impl<'de> Visitor<'de> for MessagesValueVisitor {
            type Value = MessagesValue;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("struct MessagesValue")
            }

            fn visit_seq<V>(self, mut seq: V) -> Result<MessagesValue, V::Error>
            where
                V: SeqAccess<'de>,
            {
                let messages: Vec<ChatMessage> = seq.next_element()?.unwrap_or_default();
                let mut value = MessagesValue { messages, id_index: HashMap::new() };
                value.rebuild_index();
                Ok(value)
            }

            fn visit_map<V>(self, mut map: V) -> Result<MessagesValue, V::Error>
            where
                V: MapAccess<'de>,
            {
                let mut messages: Option<Vec<ChatMessage>> = None;
                while let Some(key) = map.next_key()? {
                    match key {
                        Field::Messages => {
                            if messages.is_some() {
                                return Err(de::Error::duplicate_field("messages"));
                            }
                            messages = Some(map.next_value()?);
                        }
                    }
                }
                let messages = messages.unwrap_or_default();
                let mut value = MessagesValue { messages, id_index: HashMap::new() };
                value.rebuild_index();
                Ok(value)
            }
        }

        const FIELDS: &[&str] = &["messages"];
        deserializer.deserialize_struct("MessagesValue", FIELDS, MessagesValueVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_new_message() {
        let mut mv = MessagesValue::new();
        mv.push(ChatMessage {
            id: "msg_1".to_string(),
            role: MessageRole::User,
            content: "Hello".to_string(),
            metadata: None,
        });
        assert_eq!(mv.len(), 1);
        assert!(!mv.is_empty());
    }

    #[test]
    fn test_push_upsert_replaces() {
        let mut mv = MessagesValue::new();
        mv.push(ChatMessage {
            id: "msg_1".to_string(),
            role: MessageRole::User,
            content: "Original".to_string(),
            metadata: None,
        });
        mv.push(ChatMessage {
            id: "msg_1".to_string(),
            role: MessageRole::User,
            content: "Updated".to_string(),
            metadata: None,
        });
        assert_eq!(mv.len(), 1);
        assert_eq!(mv.iter().next().unwrap().content, "Updated");
    }

    #[test]
    fn test_extend_with_upsert() {
        let mut mv = MessagesValue::new();
        mv.push(ChatMessage {
            id: "msg_1".to_string(),
            role: MessageRole::User,
            content: "First".to_string(),
            metadata: None,
        });
        mv.extend(vec![
            ChatMessage {
                id: "msg_1".to_string(),
                role: MessageRole::User,
                content: "Updated first".to_string(),
                metadata: None,
            },
            ChatMessage {
                id: "msg_2".to_string(),
                role: MessageRole::Assistant,
                content: "Second".to_string(),
                metadata: None,
            },
        ]);
        assert_eq!(mv.len(), 2);
        let msgs: Vec<_> = mv.iter().collect();
        assert_eq!(msgs[0].content, "Updated first");
        assert_eq!(msgs[1].content, "Second");
    }

    #[test]
    fn test_by_role() {
        let mut mv = MessagesValue::new();
        mv.push(ChatMessage {
            id: "msg_1".to_string(),
            role: MessageRole::User,
            content: "User msg".to_string(),
            metadata: None,
        });
        mv.push(ChatMessage {
            id: "msg_2".to_string(),
            role: MessageRole::Assistant,
            content: "Assistant msg".to_string(),
            metadata: None,
        });
        mv.push(ChatMessage {
            id: "msg_3".to_string(),
            role: MessageRole::User,
            content: "Another user msg".to_string(),
            metadata: None,
        });

        let user_msgs = mv.by_role(MessageRole::User);
        assert_eq!(user_msgs.len(), 2);

        let assistant_msgs = mv.by_role(MessageRole::Assistant);
        assert_eq!(assistant_msgs.len(), 1);

        let system_msgs = mv.by_role(MessageRole::System);
        assert_eq!(system_msgs.is_empty(), true);
    }

    #[test]
    fn test_serialization_round_trip() {
        let mut mv = MessagesValue::new();
        mv.push(ChatMessage {
            id: "msg_1".to_string(),
            role: MessageRole::User,
            content: "Hello".to_string(),
            metadata: Some(serde_json::json!({"key": "value"})),
        });
        mv.push(ChatMessage {
            id: "msg_2".to_string(),
            role: MessageRole::Assistant,
            content: "Hi there".to_string(),
            metadata: None,
        });

        let serialized = serde_json::to_string(&mv).unwrap();
        let deserialized: MessagesValue = serde_json::from_str(&serialized).unwrap();

        assert_eq!(deserialized.len(), 2);
        let msgs: Vec<_> = deserialized.iter().collect();
        assert_eq!(msgs[0].id, "msg_1");
        assert_eq!(msgs[0].content, "Hello");
        assert_eq!(msgs[1].id, "msg_2");
        assert_eq!(msgs[1].content, "Hi there");
    }

    #[test]
    fn test_dedup_after_deserialization() {
        let mut mv = MessagesValue::new();
        mv.push(ChatMessage {
            id: "msg_1".to_string(),
            role: MessageRole::User,
            content: "Hello".to_string(),
            metadata: None,
        });

        let serialized = serde_json::to_string(&mv).unwrap();
        let mut deserialized: MessagesValue = serde_json::from_str(&serialized).unwrap();

        // After deserialization, the id_index should be rebuilt
        // so push with same ID should still deduplicate.
        deserialized.push(ChatMessage {
            id: "msg_1".to_string(),
            role: MessageRole::User,
            content: "Updated after deser".to_string(),
            metadata: None,
        });

        assert_eq!(deserialized.len(), 1);
        assert_eq!(deserialized.iter().next().unwrap().content, "Updated after deser");
    }

    #[test]
    fn test_empty_messages_value() {
        let mv = MessagesValue::new();
        assert_eq!(mv.len(), 0);
        assert!(mv.is_empty());
        assert_eq!(mv.iter().count(), 0);
        assert_eq!(mv.by_role(MessageRole::User).len(), 0);
    }
}
