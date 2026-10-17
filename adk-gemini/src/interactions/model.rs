//! Wire types for the Gemini Interactions API.
//!
//! These types map directly to the `POST /v1beta/interactions` request body and
//! the returned [`Interaction`] resource. All field names use the API's
//! `snake_case` wire contract (the Interactions API uses snake_case, unlike the
//! camelCase `generateContent` surface).

use serde::{Deserialize, Serialize};

use super::agent_config::AgentConfig;
use super::environment::Environment;

// ══════════════════════════════════════════════════════════════════════
// Content blocks (polymorphic on `type`)
// ══════════════════════════════════════════════════════════════════════

/// A polymorphic content block within a [`Step`] or interaction input.
///
/// Content blocks carry the actual payload of an interaction turn — text, images,
/// audio, documents, or video. The wire format discriminates on a `type` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    /// A text content block.
    Text(TextContent),
    /// An image content block.
    Image(ImageContent),
    /// An audio content block.
    Audio(AudioContent),
    /// A document content block (e.g. PDF).
    Document(DocumentContent),
    /// A video content block.
    Video(VideoContent),
}

impl Content {
    /// Create a plain text content block.
    pub fn text(text: impl Into<String>) -> Self {
        Content::Text(TextContent { text: text.into(), annotations: Vec::new() })
    }

    /// Create an inline (base64-encoded) image content block.
    pub fn image(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Content::Image(ImageContent {
            data: Some(data.into()),
            mime_type: Some(mime_type.into()),
            uri: None,
            resolution: None,
        })
    }

    /// Create an inline (base64-encoded) audio content block.
    pub fn audio(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Content::Audio(AudioContent {
            data: Some(data.into()),
            mime_type: Some(mime_type.into()),
            uri: None,
            sample_rate: None,
            channels: None,
        })
    }

    /// Create an inline (base64-encoded) document content block.
    pub fn document(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Content::Document(DocumentContent {
            data: Some(data.into()),
            mime_type: Some(mime_type.into()),
            uri: None,
        })
    }

    /// Create a video content block referencing a URI (e.g. a YouTube URL).
    pub fn video_uri(uri: impl Into<String>) -> Self {
        Content::Video(VideoContent {
            data: None,
            mime_type: None,
            uri: Some(uri.into()),
            resolution: None,
        })
    }

    /// Returns the text of this block if it is a [`Content::Text`].
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Content::Text(t) => Some(&t.text),
            _ => None,
        }
    }
}

/// A text content block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextContent {
    /// The text content.
    pub text: String,
    /// Citation information for model-generated content.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<serde_json::Value>,
}

/// An image content block. Either `data` (inline base64) or `uri` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageContent {
    /// Inline base64-encoded image content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// The MIME type of the image (e.g. `image/png`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// A URI referencing the image instead of inline data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// The media resolution (`low`, `medium`, `high`, `ultra_high`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
}

/// An audio content block. Either `data` (inline base64) or `uri` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioContent {
    /// Inline base64-encoded audio content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// The MIME type of the audio (e.g. `audio/wav`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// A URI referencing the audio instead of inline data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// The sample rate of the audio in Hz.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<i64>,
    /// The number of audio channels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<i64>,
}

/// A document content block (e.g. PDF). Either `data` or `uri` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentContent {
    /// Inline base64-encoded document content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// The MIME type of the document (e.g. `application/pdf`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// A URI referencing the document instead of inline data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

/// A video content block. Either `data` (inline base64) or `uri` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoContent {
    /// Inline base64-encoded video content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// The MIME type of the video (e.g. `video/mp4`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// A URI referencing the video (e.g. a public YouTube URL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// The media resolution (`low`, `medium`, `high`, `ultra_high`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
}

// ══════════════════════════════════════════════════════════════════════
// Steps (polymorphic on `type`)
// ══════════════════════════════════════════════════════════════════════

/// A single entry in an interaction's execution timeline.
///
/// Steps form the chronological record of an interaction. Input steps
/// (`user_input`), model output (`model_output`), thoughts, tool calls, and tool
/// results are all represented as typed variants. Unknown step types from future
/// API revisions deserialize into [`Step::Other`] rather than failing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    /// Input provided by the user.
    UserInput {
        /// The content blocks of the user input.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        content: Vec<Content>,
    },
    /// Output generated by the model.
    ModelOutput {
        /// The content blocks of the model output.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        content: Vec<Content>,
    },
    /// A model reasoning (thought) step.
    Thought {
        /// A signature hash for backend validation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        /// A summary of the thought.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        summary: Vec<Content>,
    },
    /// A client-side function tool call the caller must fulfil.
    FunctionCall {
        /// A unique ID for this specific tool call.
        id: String,
        /// The name of the tool to call.
        name: String,
        /// The arguments to pass to the function.
        arguments: serde_json::Value,
        /// A signature hash for backend validation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// The result of a client-side function tool call, supplied by the caller.
    FunctionResult {
        /// ID matching the originating [`Step::FunctionCall`].
        call_id: String,
        /// The name of the tool that was called.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// The result of the tool call (JSON value or string).
        result: serde_json::Value,
        /// Whether the tool call resulted in an error.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
        /// A signature hash for backend validation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// A server-side tool call or result step (code execution, search, maps,
    /// URL context, file search, MCP) that the server executes on the caller's
    /// behalf. The raw JSON is preserved so callers can render or inspect it
    /// without this crate enumerating every server tool variant.
    #[serde(untagged)]
    Other(serde_json::Value),
}

impl Step {
    /// Returns the concatenated text of a `model_output` step, if this is one.
    pub fn output_text(&self) -> Option<String> {
        match self {
            Step::ModelOutput { content } => {
                let text: String =
                    content.iter().filter_map(Content::as_text).collect::<Vec<_>>().join("");
                if text.is_empty() { None } else { Some(text) }
            }
            _ => None,
        }
    }
}

// ══════════════════════════════════════════════════════════════════════
// Tools (polymorphic on `type`)
// ══════════════════════════════════════════════════════════════════════

/// A tool the model may call during an interaction.
///
/// Function tools are the most common; built-in server-side tools (search, code
/// execution, URL context, maps, file search) are enabled by their discriminator
/// alone. Less common or future tools can be passed verbatim via [`Tool::Other`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Tool {
    /// A client-side function the model may call.
    Function {
        /// The name of the function.
        name: String,
        /// A description of the function.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// The JSON Schema for the function's parameters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parameters: Option<serde_json::Value>,
    },
    /// Built-in code execution tool.
    CodeExecution,
    /// Built-in URL context tool.
    UrlContext,
    /// Built-in Google Search grounding tool.
    GoogleSearch {
        /// The types of search grounding to enable (e.g. `web_search`).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        search_types: Vec<String>,
    },
    /// A tool variant not modelled explicitly (e.g. `computer_use`, `mcp_server`,
    /// `google_maps`, `file_search`, `retrieval`). Pass the raw JSON object.
    #[serde(untagged)]
    Other(serde_json::Value),
}

impl Tool {
    /// Create a function tool declaration.
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Tool::Function {
            name: name.into(),
            description: Some(description.into()),
            parameters: Some(parameters),
        }
    }
}

// ══════════════════════════════════════════════════════════════════════
// Generation config & response format
// ══════════════════════════════════════════════════════════════════════

/// Whether to include thought summaries in the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingSummaries {
    /// Let the model decide whether to emit thought summaries.
    Auto,
    /// Never emit thought summaries.
    None,
}

/// Tool-choice configuration controlling whether/how the model calls tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoice {
    /// The model decides whether to call tools.
    Auto,
    /// The model must call at least one tool.
    Any,
    /// The model must not call tools.
    None,
    /// Tool calls are validated against declarations.
    Validated,
}

/// Image output configuration for the Interactions `generation_config`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageConfig {
    /// The aspect ratio (e.g. `1:1`, `16:9`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aspect_ratio: Option<String>,
    /// The image size (`512`, `1K`, `2K`, `4K`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_size: Option<String>,
}

/// Configuration parameters for a model interaction.
///
/// This is the Interactions API equivalent of `generateContent`'s
/// `GenerationConfig`. Note that `thinking_level` uses the same string enum as
/// the rest of the crate ([`crate::ThinkingLevel`]), and sampling parameters
/// (`temperature`, `top_p`) are discouraged for Gemini 3.x models.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GenerationConfig {
    /// The maximum number of tokens to include in the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i32>,
    /// Controls the randomness of the output. Discouraged for Gemini 3.x.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Nucleus sampling cumulative probability. Discouraged for Gemini 3.x.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Seed used in decoding for reproducibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    /// A list of character sequences that stop generation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    /// The reasoning effort level (`minimal`, `low`, `medium`, `high`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<crate::ThinkingLevel>,
    /// Whether to include thought summaries in the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_summaries: Option<ThinkingSummaries>,
    /// The tool-choice configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// Configuration for image output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_config: Option<ImageConfig>,
}

impl GenerationConfig {
    /// Validate the generation configuration.
    ///
    /// Returns an error if `temperature` or `top_p` fall outside their valid
    /// ranges, or if `max_output_tokens` is non-positive.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(t) = self.temperature
            && !(0.0..=2.0).contains(&t)
        {
            return Err("temperature must be between 0.0 and 2.0".to_string());
        }
        if let Some(p) = self.top_p
            && !(0.0..=1.0).contains(&p)
        {
            return Err("top_p must be between 0.0 and 1.0".to_string());
        }
        if let Some(m) = self.max_output_tokens
            && m <= 0
        {
            return Err("max_output_tokens must be positive".to_string());
        }
        Ok(())
    }
}

/// The output format constraint for an interaction.
///
/// Replaces `generateContent`'s `response_mime_type` + `response_schema`. The
/// polymorphic form lets callers request structured JSON (`text` with a schema),
/// image, or audio output. To request multiple modalities, pass an array via
/// [`CreateInteractionRequest::response_format_list`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    /// Text output, optionally constrained to a JSON schema.
    Text {
        /// The MIME type (`application/json` or `text/plain`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        /// The JSON schema the output must conform to (JSON mode only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        schema: Option<serde_json::Value>,
    },
    /// Image output configuration.
    Image {
        /// The MIME type of the image output (e.g. `image/jpeg`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        /// The aspect ratio for the image output.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        aspect_ratio: Option<String>,
        /// The size of the image output (`512`, `1K`, `2K`, `4K`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image_size: Option<String>,
    },
    /// Audio output configuration.
    Audio {
        /// The MIME type of the audio output (e.g. `audio/wav`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        /// Sample rate in Hz.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sample_rate: Option<i64>,
    },
}

impl ResponseFormat {
    /// Convenience constructor for structured JSON output with a schema.
    pub fn json_schema(schema: serde_json::Value) -> Self {
        ResponseFormat::Text {
            mime_type: Some("application/json".to_string()),
            schema: Some(schema),
        }
    }
}

/// A requested output modality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponseModality {
    /// Text output.
    Text,
    /// Image output.
    Image,
    /// Audio output.
    Audio,
    /// Video output.
    Video,
    /// Document output.
    Document,
}

/// The service tier for an interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceTier {
    /// Flex (lowest cost, best-effort latency).
    Flex,
    /// Standard tier.
    Standard,
    /// Priority (lowest latency).
    Priority,
}

// ══════════════════════════════════════════════════════════════════════
// Input
// ══════════════════════════════════════════════════════════════════════

/// The polymorphic `input` field of a create-interaction request.
///
/// The Interactions API accepts a bare string, a single content block, a list of
/// content blocks, or a list of steps (for stateless multi-turn history).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Input {
    /// A bare text prompt.
    Text(String),
    /// A list of content blocks (single turn, possibly multimodal).
    Content(Vec<Content>),
    /// A list of steps (stateless multi-turn history).
    Steps(Vec<Step>),
}

impl Default for Input {
    fn default() -> Self {
        Input::Content(Vec::new())
    }
}

impl From<String> for Input {
    fn from(s: String) -> Self {
        Input::Text(s)
    }
}

impl From<&str> for Input {
    fn from(s: &str) -> Self {
        Input::Text(s.to_string())
    }
}

impl From<Vec<Content>> for Input {
    fn from(c: Vec<Content>) -> Self {
        Input::Content(c)
    }
}

impl From<Vec<Step>> for Input {
    fn from(s: Vec<Step>) -> Self {
        Input::Steps(s)
    }
}

// ══════════════════════════════════════════════════════════════════════
// Request
// ══════════════════════════════════════════════════════════════════════

/// The request body for `POST /v1beta/interactions`.
///
/// Construct this via [`InteractionBuilder`](crate::interactions::InteractionBuilder)
/// rather than by hand in most cases.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateInteractionRequest {
    /// The model to use (required if `agent` is not set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The agent to use (required if `model` is not set), e.g. a Deep Research agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The interaction input.
    pub input: Input,
    /// System instruction for the interaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<String>,
    /// Tool declarations the model may call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    /// A single response-format constraint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    /// A list of response-format constraints (multi-modal output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format_list: Option<Vec<ResponseFormat>>,
    /// Requested output modalities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub response_modalities: Vec<ResponseModality>,
    /// Whether the interaction will be streamed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Whether to store the interaction for later retrieval (default server-side: true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    /// Whether to run the interaction in the background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    /// Model generation configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GenerationConfig>,
    /// The ID of the previous interaction (server-side history continuation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_interaction_id: Option<String>,
    /// The service tier for the interaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
    /// Environment configuration: fresh sandbox, resume by ID, or inline config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<Environment>,
    /// Managed-agent-specific configuration (e.g. Deep Research options).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_config: Option<AgentConfig>,
}

// ══════════════════════════════════════════════════════════════════════
// Response (Interaction resource)
// ══════════════════════════════════════════════════════════════════════

/// The lifecycle status of an interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionStatus {
    /// The interaction is still running.
    InProgress,
    /// The interaction is paused awaiting a tool result from the caller.
    RequiresAction,
    /// The interaction finished successfully.
    Completed,
    /// The interaction failed.
    Failed,
    /// The interaction was cancelled.
    Cancelled,
    /// The interaction stopped before completion.
    Incomplete,
    /// The interaction exceeded its budget.
    BudgetExceeded,
}

impl InteractionStatus {
    /// Returns `true` if the interaction has reached a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            InteractionStatus::Completed
                | InteractionStatus::Failed
                | InteractionStatus::Cancelled
                | InteractionStatus::Incomplete
                | InteractionStatus::BudgetExceeded
        )
    }

    /// Returns `true` if the interaction is waiting for the caller to supply a
    /// function result (client-side tool call).
    pub fn requires_action(&self) -> bool {
        matches!(self, InteractionStatus::RequiresAction)
    }
}

/// Token usage broken down by modality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModalityTokens {
    /// The modality the token count applies to.
    pub modality: ResponseModality,
    /// Number of tokens for the modality; zero when the API reports `null` or leaves it out.
    #[serde(default, deserialize_with = "adk_core::nullable_count")]
    pub tokens: i64,
}

/// Token usage statistics for an interaction.
///
/// A count the API reports as `null` or leaves out reads as zero instead of failing the
/// interaction, and [`raw`](Self::raw) keeps the object exactly as it was sent.
///
/// # Example
///
/// ```
/// use adk_gemini::interactions::Usage;
///
/// let usage: Usage = serde_json::from_value(serde_json::json!({
///     "total_input_tokens": null,
///     "total_output_tokens": 9,
///     "total_tool_use_tokens": null
/// }))
/// .unwrap();
/// assert_eq!((usage.total_input_tokens, usage.total_output_tokens), (0, 9));
/// assert!(usage.raw.as_ref().unwrap()["total_input_tokens"].is_null());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "serde_json::Value")]
pub struct Usage {
    /// Total input (prompt) tokens.
    pub total_input_tokens: i64,
    /// Total output (response) tokens.
    pub total_output_tokens: i64,
    /// Total thought tokens (thinking models).
    pub total_thought_tokens: i64,
    /// Total cached tokens.
    pub total_cached_tokens: i64,
    /// Total tool-use tokens.
    pub total_tool_use_tokens: i64,
    /// Total token count for the interaction.
    pub total_tokens: i64,
    /// Input tokens broken down by modality.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub input_tokens_by_modality: Vec<ModalityTokens>,
    /// Output tokens broken down by modality.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub output_tokens_by_modality: Vec<ModalityTokens>,
    /// The `usage` object exactly as the API sent it, including fields this type does not
    /// model. `None` for a value built in code.
    #[serde(skip)]
    pub raw: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct UsageWire {
    #[serde(default, deserialize_with = "adk_core::nullable_count")]
    total_input_tokens: i64,
    #[serde(default, deserialize_with = "adk_core::nullable_count")]
    total_output_tokens: i64,
    #[serde(default, deserialize_with = "adk_core::nullable_count")]
    total_thought_tokens: i64,
    #[serde(default, deserialize_with = "adk_core::nullable_count")]
    total_cached_tokens: i64,
    #[serde(default, deserialize_with = "adk_core::nullable_count")]
    total_tool_use_tokens: i64,
    #[serde(default, deserialize_with = "adk_core::nullable_count")]
    total_tokens: i64,
    #[serde(default, deserialize_with = "adk_core::nullable_count")]
    input_tokens_by_modality: Vec<ModalityTokens>,
    #[serde(default, deserialize_with = "adk_core::nullable_count")]
    output_tokens_by_modality: Vec<ModalityTokens>,
}

impl TryFrom<serde_json::Value> for Usage {
    type Error = serde_json::Error;

    fn try_from(raw: serde_json::Value) -> Result<Self, Self::Error> {
        let wire = UsageWire::deserialize(&raw)?;
        Ok(Self {
            total_input_tokens: wire.total_input_tokens,
            total_output_tokens: wire.total_output_tokens,
            total_thought_tokens: wire.total_thought_tokens,
            total_cached_tokens: wire.total_cached_tokens,
            total_tool_use_tokens: wire.total_tool_use_tokens,
            total_tokens: wire.total_tokens,
            input_tokens_by_modality: wire.input_tokens_by_modality,
            output_tokens_by_modality: wire.output_tokens_by_modality,
            raw: Some(raw),
        })
    }
}

/// The `Interaction` resource returned by create/get/cancel.
///
/// An interaction is the unit of work in the Interactions API. It carries the
/// full [`Step`] timeline, lifecycle [`status`](Interaction::status), token
/// [`usage`](Interaction::usage), and the server-assigned [`id`](Interaction::id)
/// used to continue the conversation via `previous_interaction_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Interaction {
    /// A unique identifier for the interaction.
    #[serde(default)]
    pub id: String,
    /// The model that produced the interaction, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The agent that produced the interaction, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The lifecycle status of the interaction.
    #[serde(default = "default_status")]
    pub status: InteractionStatus,
    /// The execution-step timeline.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Token usage statistics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// ISO 8601 creation time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// ISO 8601 last-updated time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    /// The server-assigned environment ID, if an environment was attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
}

fn default_status() -> InteractionStatus {
    InteractionStatus::InProgress
}

impl Interaction {
    /// Returns the model's final text output.
    ///
    /// Mirrors the SDK `output_text` convenience property: returns the
    /// concatenated text of the last `model_output` step, or `None` if the
    /// final step is not text (e.g. a tool call or image output).
    pub fn output_text(&self) -> Option<String> {
        self.steps.iter().rev().find_map(Step::output_text)
    }

    /// Returns the pending client-side function calls, if the interaction is
    /// waiting on tool results (`status == requires_action`).
    ///
    /// Each returned tuple is `(call_id, name, arguments)`. Supply the results
    /// via [`InteractionBuilder::function_result`](crate::interactions::InteractionBuilder::function_result)
    /// in a follow-up request that references this interaction's `id`.
    pub fn pending_function_calls(&self) -> Vec<(String, String, serde_json::Value)> {
        self.steps
            .iter()
            .filter_map(|step| match step {
                Step::FunctionCall { id, name, arguments, .. } => {
                    Some((id.clone(), name.clone(), arguments.clone()))
                }
                _ => None,
            })
            .collect()
    }
}
