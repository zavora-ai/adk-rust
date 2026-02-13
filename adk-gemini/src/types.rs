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

use serde::{Deserialize, Serialize};

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
        /// The thought signature for the text (Gemini 2.5 series only)
        #[serde(rename = "thoughtSignature", skip_serializing_if = "Option::is_none")]
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
        /// The thought signature for the function call (Gemini 2.5 series only)
        #[serde(rename = "thoughtSignature", skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    /// Function response (results from executing a function call)
    FunctionResponse {
        /// The function response details
        #[serde(rename = "functionResponse")]
        function_response: super::tools::FunctionResponse,
    },
    /// Code execution result (from Gemini code execution)
    CodeExecutionResult {
        /// The code execution result details
        #[serde(rename = "codeExecutionResult")]
        code_execution_result: CodeExecutionResultData,
    },
}

/// Result from code execution in Gemini
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CodeExecutionResultData {
    /// Outcome of the execution (e.g. "OUTCOME_OK", "OUTCOME_DEADLINE_EXCEEDED")
    pub outcome: String,
    /// Output from the execution
    pub output: String,
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
        Self { parts: Some(vec![Part::FunctionResponse { function_response }]), role: None }
    }

    /// Create a new content with a function response from name and JSON value
    pub fn function_response_json(name: impl Into<String>, response: serde_json::Value) -> Self {
        Self {
            parts: Some(vec![Part::FunctionResponse {
                function_response: super::tools::FunctionResponse::new(name, response),
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

hybrid_enum! {
    /// Content modality type — specifies the format of model output
    pub enum Modality {
        /// Default value.
        ModalityUnspecified => ("MODALITY_UNSPECIFIED", 0),
        /// Indicates the model should return text.
        Text                => ("TEXT", 1),
        /// Indicates the model should return images.
        Image               => ("IMAGE", 2),
        /// Indicates the model should return video.
        Video               => ("VIDEO", 3),
        /// Indicates the model should return audio.
        Audio               => ("AUDIO", 4),
        /// Indicates document content (PDFs, etc.)
        Document            => ("DOCUMENT", 5),
        /// Unknown or future modality types.
        Unknown             => ("UNKNOWN", 99),
    }
    fallback: Unknown
}

/// Vertex AI Context (moved from being internal to Public for shared use)
#[derive(Debug, Clone)]
#[cfg(feature = "vertex")]
pub struct VertexContext {
    pub project: String,
    pub location: String,
    pub token: String, // OAuth token
}

/// Configuration for Gemini Live backend (Public or Vertex)
/// This is used by adk-realtime to determine how to connect.
#[derive(Debug, Clone)]
pub enum GeminiLiveBackend {
    /// Public API (Google AI Studio)
    Studio {
        /// API Key
        api_key: String,
    },
    /// Vertex AI (Google Cloud) - Pre-authenticated
    #[cfg(feature = "vertex")]
    Vertex(VertexContext),
    /// Vertex AI (Google Cloud) - ADC (Application Default Credentials)
    #[cfg(feature = "vertex")]
    VertexADC {
        /// Google Cloud Project ID
        project: String,
        /// Google Cloud Location (e.g., "us-central1")
        location: String,
    },
}
