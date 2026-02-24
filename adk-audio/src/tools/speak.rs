//! SpeakTool — synthesize text to speech via a configured TtsProvider.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::traits::{TtsProvider, TtsRequest};

/// Tool that synthesizes text to speech audio.
///
/// Accepts JSON `{text, voice?, emotion?}` and returns `{duration_ms, sample_rate}`.
pub struct SpeakTool {
    tts: Arc<dyn TtsProvider>,
    default_voice: String,
}

impl SpeakTool {
    /// Create a new `SpeakTool` with the given TTS provider and default voice.
    pub fn new(tts: Arc<dyn TtsProvider>, default_voice: impl Into<String>) -> Self {
        Self { tts, default_voice: default_voice.into() }
    }
}

#[async_trait]
impl adk_core::Tool for SpeakTool {
    fn name(&self) -> &str {
        "speak"
    }

    fn description(&self) -> &str {
        "Synthesize text to speech audio"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "Text to speak" },
                "voice": { "type": "string", "description": "Voice ID (optional)" },
                "emotion": { "type": "string", "enum": ["neutral","happy","sad","angry","whisper","excited","calm"] }
            },
            "required": ["text"]
        }))
    }

    async fn execute(
        &self,
        _ctx: Arc<dyn adk_core::ToolContext>,
        args: Value,
    ) -> adk_core::Result<Value> {
        let text = args["text"].as_str().unwrap_or_default();
        let voice = args["voice"].as_str().unwrap_or(&self.default_voice).to_string();
        let request = TtsRequest { text: text.into(), voice, ..Default::default() };
        let frame = self
            .tts
            .synthesize(&request)
            .await
            .map_err(|e| adk_core::AdkError::Tool(format!("speak: {e}")))?;
        Ok(serde_json::json!({
            "duration_ms": frame.duration_ms,
            "sample_rate": frame.sample_rate
        }))
    }
}
