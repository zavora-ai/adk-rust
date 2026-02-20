//! Configuration types for realtime sessions.

use crate::audio::AudioEncoding;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Voice Activity Detection mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VadMode {
    /// Server-side VAD (default for most providers).
    #[default]
    ServerVad,
    /// Semantic VAD (OpenAI-specific).
    SemanticVad,
    /// No automatic VAD - manual turn management.
    None,
}

/// VAD configuration options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VadConfig {
    /// VAD mode to use.
    #[serde(rename = "type")]
    pub mode: VadMode,
    /// Silence duration (ms) before considering speech ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silence_duration_ms: Option<u32>,
    /// Detection threshold (0.0 - 1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f32>,
    /// Prefix padding (ms) to include before detected speech.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_padding_ms: Option<u32>,
    /// Whether to interrupt the model when user starts speaking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interrupt_response: Option<bool>,
    /// Eagerness of turn detection (OpenAI-specific).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eagerness: Option<String>,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            mode: VadMode::ServerVad,
            silence_duration_ms: Some(500),
            threshold: None,
            prefix_padding_ms: None,
            interrupt_response: Some(true),
            eagerness: None,
        }
    }
}

impl VadConfig {
    /// Create a server VAD config with default settings.
    pub fn server_vad() -> Self {
        Self::default()
    }

    /// Create a semantic VAD config (OpenAI).
    pub fn semantic_vad() -> Self {
        Self { mode: VadMode::SemanticVad, ..Default::default() }
    }

    /// Create a config with VAD disabled.
    pub fn disabled() -> Self {
        Self { mode: VadMode::None, ..Default::default() }
    }

    /// Set silence duration threshold.
    pub fn with_silence_duration(mut self, ms: u32) -> Self {
        self.silence_duration_ms = Some(ms);
        self
    }

    /// Set whether to interrupt on user speech.
    pub fn with_interrupt(mut self, interrupt: bool) -> Self {
        self.interrupt_response = Some(interrupt);
        self
    }
}

/// Tool/function definition for realtime sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool name.
    pub name: String,
    /// Tool description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
}

impl ToolDefinition {
    /// Create a new tool definition.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), description: None, parameters: None }
    }

    /// Set the tool description.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Set the parameters schema.
    pub fn with_parameters(mut self, schema: Value) -> Self {
        self.parameters = Some(schema);
        self
    }
}

/// Configuration for a realtime session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RealtimeConfig {
    /// Model to use (provider-specific).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// System instruction for the agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,

    /// Voice to use for audio output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,

    /// Output modalities: ["text"], ["audio"], or ["text", "audio"].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modalities: Option<Vec<String>>,

    /// Input audio format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_audio_format: Option<AudioEncoding>,

    /// Output audio format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_audio_format: Option<AudioEncoding>,

    /// VAD configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_detection: Option<VadConfig>,

    /// Available tools/functions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,

    /// Tool selection mode: "auto", "none", "required".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,

    /// Whether to include input audio transcription.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_audio_transcription: Option<TranscriptionConfig>,

    /// Temperature for response generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Maximum output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_response_output_tokens: Option<u32>,

    /// Cached content resource name (e.g. `cachedContents/1234`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_content: Option<String>,

    /// Provider-specific options.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

/// Transcription configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionConfig {
    /// Transcription model to use.
    pub model: String,
}

impl TranscriptionConfig {
    /// Use whisper-1 for transcription.
    pub fn whisper() -> Self {
        Self { model: "whisper-1".to_string() }
    }
}

impl RealtimeConfig {
    /// Create a new empty configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a builder for RealtimeConfig.
    pub fn builder() -> RealtimeConfigBuilder {
        RealtimeConfigBuilder::new()
    }

    /// Set the model.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Set the system instruction.
    pub fn with_instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    /// Set the voice.
    pub fn with_voice(mut self, voice: impl Into<String>) -> Self {
        self.voice = Some(voice.into());
        self
    }

    /// Set output modalities.
    pub fn with_modalities(mut self, modalities: Vec<String>) -> Self {
        self.modalities = Some(modalities);
        self
    }

    /// Enable text and audio output.
    pub fn with_text_and_audio(mut self) -> Self {
        self.modalities = Some(vec!["text".to_string(), "audio".to_string()]);
        self
    }

    /// Enable audio-only output.
    pub fn with_audio_only(mut self) -> Self {
        self.modalities = Some(vec!["audio".to_string()]);
        self
    }

    /// Set VAD configuration.
    pub fn with_vad(mut self, vad: VadConfig) -> Self {
        self.turn_detection = Some(vad);
        self
    }

    /// Enable server-side VAD with default settings.
    pub fn with_server_vad(self) -> Self {
        self.with_vad(VadConfig::server_vad())
    }

    /// Disable VAD (manual turn management).
    pub fn without_vad(mut self) -> Self {
        self.turn_detection = Some(VadConfig::disabled());
        self
    }

    /// Add a tool definition.
    pub fn with_tool(mut self, tool: ToolDefinition) -> Self {
        self.tools.get_or_insert_with(Vec::new).push(tool);
        self
    }

    /// Set multiple tools.
    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Enable input audio transcription.
    pub fn with_transcription(mut self) -> Self {
        self.input_audio_transcription = Some(TranscriptionConfig::whisper());
        self
    }

    /// Set temperature.
    pub fn with_temperature(mut self, temp: f32) -> Self {
        self.temperature = Some(temp);
        self
    }

    /// Set cached content resource.
    pub fn with_cached_content(mut self, content: impl Into<String>) -> Self {
        self.cached_content = Some(content.into());
        self
    }
}

/// Builder for RealtimeConfig.
#[derive(Debug, Clone, Default)]
pub struct RealtimeConfigBuilder {
    config: RealtimeConfig,
}

impl RealtimeConfigBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the model.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.config.model = Some(model.into());
        self
    }

    /// Set the system instruction.
    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.config.instruction = Some(instruction.into());
        self
    }

    /// Set the voice.
    pub fn voice(mut self, voice: impl Into<String>) -> Self {
        self.config.voice = Some(voice.into());
        self
    }

    /// Enable VAD.
    pub fn vad_enabled(mut self, enabled: bool) -> Self {
        if enabled {
            self.config.turn_detection = Some(VadConfig::server_vad());
        } else {
            self.config.turn_detection = Some(VadConfig::disabled());
        }
        self
    }

    /// Set VAD configuration.
    pub fn vad(mut self, vad: VadConfig) -> Self {
        self.config.turn_detection = Some(vad);
        self
    }

    /// Add a tool.
    pub fn tool(mut self, tool: ToolDefinition) -> Self {
        self.config.tools.get_or_insert_with(Vec::new).push(tool);
        self
    }

    /// Set temperature.
    pub fn temperature(mut self, temp: f32) -> Self {
        self.config.temperature = Some(temp);
        self
    }

    /// Set cached content resource.
    pub fn cached_content(mut self, content: impl Into<String>) -> Self {
        self.config.cached_content = Some(content.into());
        self
    }

    /// Build the configuration.
    pub fn build(self) -> RealtimeConfig {
        self.config
    }
}
