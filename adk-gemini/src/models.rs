//! # Core Gemini API Primitives
//!
//! This module contains the fundamental building blocks used across the Gemini API.
//! These core data structures are shared by multiple modules and form the foundation
//! for constructing requests and parsing responses.
//!
//! ## Core Types
//!
//! - [`Role`] - Represents the speaker in a conversation (User or Model)
//! - [`Part`] - Content fragments that make up messages (text, images, function calls)
//! - [`Blob`] - Binary data with MIME type for inline content
//! - [`Content`] - Container for parts with optional role assignment
//! - [`Message`] - Complete message with content and explicit role
//! - [`Modality`] - Output format types (text, image, audio)
//!
//! ## Usage
//!
//! These types are typically used in combination with the domain-specific modules:
//! - `generation` - For content generation requests and responses
//! - `embedding` - For text embedding operations
//! - `safety` - For content moderation settings
//! - `tools` - For function calling capabilities
//! - `batch` - For batch processing operations
//! - `cache` - For content caching
//! - `files` - For file management

#![allow(clippy::enum_variant_names)]

use serde::{Deserialize, Serialize, de};

/// Role of a message in a conversation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Message from the user
    User,
    /// Message from the model
    Model,
}

/// Content part that can be included in a message
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Part {
    /// Text content
    Text {
        /// The text content
        text: String,
        /// Whether this is a thought summary (Gemini 2.5 series only)
        #[serde(skip_serializing_if = "Option::is_none")]
        thought: Option<bool>,
        /// The thought signature (Gemini 2.5+ thinking models only).
        /// Preserved from responses and echoed back in conversation history for Gemini 3.x thought signature support.
        #[serde(rename = "thoughtSignature", default, skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    InlineData {
        /// The blob data
        #[serde(rename = "inlineData")]
        inline_data: Blob,
    },
    /// Function call from the model
    FunctionCall {
        /// The function call details
        #[serde(rename = "functionCall")]
        function_call: super::tools::FunctionCall,
        /// The thought signature (Gemini 2.5+ thinking models only).
        /// Preserved from responses and echoed back in conversation history for Gemini 3.x thought signature support.
        #[serde(rename = "thoughtSignature", default, skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    /// Function response (results from executing a function call)
    FunctionResponse {
        /// The function response details
        #[serde(rename = "functionResponse")]
        function_response: super::tools::FunctionResponse,
        /// The thought signature (Gemini 3.x thinking models).
        /// Must be echoed back on function response parts when thinking is active.
        #[serde(rename = "thoughtSignature", default, skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    /// Server-side tool call from Gemini 3 (built-in tool invocation)
    ToolCall {
        #[serde(rename = "toolCall")]
        tool_call: serde_json::Value,
        /// The thought signature (Gemini 3.x thinking models).
        /// Must be preserved and echoed back in conversation history.
        #[serde(rename = "thoughtSignature", default, skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    /// Server-side tool response from Gemini 3 (built-in tool result)
    ToolResponse {
        #[serde(rename = "toolResponse")]
        tool_response: serde_json::Value,
        /// The thought signature (Gemini 3.x thinking models).
        /// Must be preserved and echoed back in conversation history.
        #[serde(rename = "thoughtSignature", default, skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    /// Generated code emitted by Gemini code execution.
    ExecutableCode {
        #[serde(rename = "executableCode")]
        executable_code: serde_json::Value,
        /// The thought signature (Gemini 3.x thinking models).
        /// Must be preserved and echoed back in conversation history.
        #[serde(rename = "thoughtSignature", default, skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    /// Result emitted by Gemini code execution.
    CodeExecutionResult {
        #[serde(rename = "codeExecutionResult")]
        code_execution_result: serde_json::Value,
        /// The thought signature (Gemini 3.x thinking models).
        /// Must be preserved and echoed back in conversation history.
        #[serde(rename = "thoughtSignature", default, skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
}

/// Blob for a message part
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Blob {
    /// The MIME type of the data
    pub mime_type: String,
    /// Base64 encoded data
    pub data: String,
}

impl Blob {
    /// Create a new blob with mime type and data
    pub fn new(mime_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self { mime_type: mime_type.into(), data: data.into() }
    }
}

/// Content of a message
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Content {
    /// Parts of the content
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<Part>>,
    /// Role of the content
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
}

impl Content {
    /// Create a new text content
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            parts: Some(vec![Part::Text {
                text: text.into(),
                thought: None,
                thought_signature: None,
            }]),
            role: None,
        }
    }

    /// Create a new content with a function call
    pub fn function_call(function_call: super::tools::FunctionCall) -> Self {
        Self {
            parts: Some(vec![Part::FunctionCall { function_call, thought_signature: None }]),
            role: None,
        }
    }

    /// Create a new content with a function call and thought signature
    pub fn function_call_with_thought(
        function_call: super::tools::FunctionCall,
        thought_signature: impl Into<String>,
    ) -> Self {
        Self {
            parts: Some(vec![Part::FunctionCall {
                function_call,
                thought_signature: Some(thought_signature.into()),
            }]),
            role: None,
        }
    }

    /// Create a new text content with thought signature
    pub fn text_with_thought_signature(
        text: impl Into<String>,
        thought_signature: impl Into<String>,
    ) -> Self {
        Self {
            parts: Some(vec![Part::Text {
                text: text.into(),
                thought: None,
                thought_signature: Some(thought_signature.into()),
            }]),
            role: None,
        }
    }

    /// Create a new thought content with thought signature
    pub fn thought_with_signature(
        text: impl Into<String>,
        thought_signature: impl Into<String>,
    ) -> Self {
        Self {
            parts: Some(vec![Part::Text {
                text: text.into(),
                thought: Some(true),
                thought_signature: Some(thought_signature.into()),
            }]),
            role: None,
        }
    }

    /// Create a new content with a function response
    pub fn function_response(function_response: super::tools::FunctionResponse) -> Self {
        Self {
            parts: Some(vec![Part::FunctionResponse {
                function_response,
                thought_signature: None,
            }]),
            role: None,
        }
    }

    /// Create a new content with a function response from name and JSON value
    pub fn function_response_json(name: impl Into<String>, response: serde_json::Value) -> Self {
        Self {
            parts: Some(vec![Part::FunctionResponse {
                function_response: super::tools::FunctionResponse::new(name, response),
                thought_signature: None,
            }]),
            role: None,
        }
    }

    /// Create a new content with inline data (blob data)
    pub fn inline_data(mime_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            parts: Some(vec![Part::InlineData { inline_data: Blob::new(mime_type, data) }]),
            role: None,
        }
    }

    /// Add a role to this content
    pub fn with_role(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }
}

/// Message in a conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Content of the message
    pub content: Content,
    /// Role of the message
    pub role: Role,
}

impl Message {
    /// Create a new user message with text content
    pub fn user(text: impl Into<String>) -> Self {
        Self { content: Content::text(text).with_role(Role::User), role: Role::User }
    }

    /// Create a new model message with text content
    pub fn model(text: impl Into<String>) -> Self {
        Self { content: Content::text(text).with_role(Role::Model), role: Role::Model }
    }

    /// Create a new embedding message with text content
    pub fn embed(text: impl Into<String>) -> Self {
        Self { content: Content::text(text), role: Role::Model }
    }

    /// Create a new function message with function response content from JSON
    pub fn function(name: impl Into<String>, response: serde_json::Value) -> Self {
        Self {
            content: Content::function_response_json(name, response).with_role(Role::Model),
            role: Role::Model,
        }
    }

    /// Create a new function message with function response from a JSON string
    pub fn function_str(
        name: impl Into<String>,
        response: impl Into<String>,
    ) -> Result<Self, serde_json::Error> {
        let response_str = response.into();
        let json = serde_json::from_str(&response_str)?;
        Ok(Self {
            content: Content::function_response_json(name, json).with_role(Role::Model),
            role: Role::Model,
        })
    }
}

/// Content modality type - specifies the format of model output
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Modality {
    /// Default value.
    ModalityUnspecified,
    /// Indicates the model should return text.
    Text,
    /// Indicates the model should return images.
    Image,
    /// Indicates the model should return audio.
    Audio,
    /// Indicates the model should return video.
    Video,
    /// Indicates document content (PDFs, etc.)
    Document,
    /// Unknown or future modality types
    Unknown,
}

impl Modality {
    fn from_wire_str(value: &str) -> Self {
        match value {
            "MODALITY_UNSPECIFIED" => Self::ModalityUnspecified,
            "TEXT" => Self::Text,
            "IMAGE" => Self::Image,
            "AUDIO" => Self::Audio,
            "VIDEO" => Self::Video,
            "DOCUMENT" => Self::Document,
            _ => Self::Unknown,
        }
    }

    fn from_wire_number(value: i64) -> Self {
        match value {
            0 => Self::ModalityUnspecified,
            1 => Self::Text,
            2 => Self::Image,
            3 => Self::Video,
            4 => Self::Audio,
            5 => Self::Document,
            _ => Self::Unknown,
        }
    }
}

impl<'de> Deserialize<'de> for Modality {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::String(s) => Ok(Self::from_wire_str(&s)),
            serde_json::Value::Number(n) => n
                .as_i64()
                .map(Self::from_wire_number)
                .ok_or_else(|| de::Error::custom("modality must be an integer-compatible number")),
            _ => Err(de::Error::custom("modality must be a string or integer")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_call_deserialize_and_roundtrip() {
        let json = r#"{"toolCall": {"name": "google_search", "args": {"query": "rust lang"}}}"#;
        let part: Part = serde_json::from_str(json).expect("should deserialize toolCall");
        match &part {
            Part::ToolCall { tool_call, .. } => {
                assert_eq!(tool_call["name"], "google_search");
                assert_eq!(tool_call["args"]["query"], "rust lang");
            }
            other => panic!("expected Part::ToolCall, got {other:?}"),
        }
        // Round-trip
        let serialized = serde_json::to_string(&part).expect("should serialize");
        let deserialized: Part =
            serde_json::from_str(&serialized).expect("should deserialize again");
        assert_eq!(part, deserialized);
    }

    #[test]
    fn test_tool_response_deserialize_and_roundtrip() {
        let json = r#"{"toolResponse": {"name": "google_search", "output": {"results": []}}, "thoughtSignature": "sig_123"}"#;
        let part: Part = serde_json::from_str(json).expect("should deserialize toolResponse");
        match &part {
            Part::ToolResponse { tool_response, thought_signature } => {
                assert_eq!(tool_response["name"], "google_search");
                assert_eq!(tool_response["output"]["results"], serde_json::json!([]));
                assert_eq!(thought_signature.as_deref(), Some("sig_123"));
            }
            other => panic!("expected Part::ToolResponse, got {other:?}"),
        }
        // Round-trip
        let serialized = serde_json::to_string(&part).expect("should serialize");
        let deserialized: Part =
            serde_json::from_str(&serialized).expect("should deserialize again");
        assert_eq!(part, deserialized);
    }

    #[test]
    fn test_code_execution_parts_preserve_thought_signature() {
        let executable = serde_json::json!({
            "executableCode": { "language": "python", "code": "print(1)" },
            "thoughtSignature": "sig_exec"
        });
        let result = serde_json::json!({
            "codeExecutionResult": { "outcome": "OUTCOME_OK", "output": "1" },
            "thoughtSignature": "sig_result"
        });

        let executable_part: Part =
            serde_json::from_value(executable).expect("should deserialize executable code");
        let result_part: Part =
            serde_json::from_value(result).expect("should deserialize code execution result");

        match executable_part {
            Part::ExecutableCode { thought_signature, .. } => {
                assert_eq!(thought_signature.as_deref(), Some("sig_exec"));
            }
            other => panic!("expected Part::ExecutableCode, got {other:?}"),
        }

        match result_part {
            Part::CodeExecutionResult { thought_signature, .. } => {
                assert_eq!(thought_signature.as_deref(), Some("sig_result"));
            }
            other => panic!("expected Part::CodeExecutionResult, got {other:?}"),
        }
    }
}
