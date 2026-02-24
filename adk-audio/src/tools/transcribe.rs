//! TranscribeTool — transcribe audio via a configured SttProvider.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::traits::{SttOptions, SttProvider};

/// Tool that transcribes audio to text.
///
/// Accepts JSON referencing an audio artifact and returns `{text, confidence}`.
pub struct TranscribeTool {
    stt: Arc<dyn SttProvider>,
}

impl TranscribeTool {
    /// Create a new `TranscribeTool` with the given STT provider.
    pub fn new(stt: Arc<dyn SttProvider>) -> Self {
        Self { stt }
    }
}

#[async_trait]
impl adk_core::Tool for TranscribeTool {
    fn name(&self) -> &str {
        "transcribe"
    }

    fn description(&self) -> &str {
        "Transcribe audio to text"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "audio_data": { "type": "string", "description": "Base64-encoded PCM16 audio data" },
                "sample_rate": { "type": "integer", "description": "Sample rate in Hz (default 16000)" },
                "language": { "type": "string", "description": "BCP-47 language hint (optional)" }
            },
            "required": ["audio_data"]
        }))
    }

    async fn execute(
        &self,
        _ctx: Arc<dyn adk_core::ToolContext>,
        args: Value,
    ) -> adk_core::Result<Value> {
        let audio_b64 = args["audio_data"].as_str().unwrap_or_default();
        let sample_rate = args["sample_rate"].as_u64().unwrap_or(16000) as u32;
        let language = args["language"].as_str().map(String::from);

        // Decode base64 audio
        use bytes::Bytes;
        let data = base64_decode(audio_b64)
            .map_err(|e| adk_core::AdkError::Tool(format!("transcribe: invalid base64: {e}")))?;
        let frame = crate::frame::AudioFrame::new(Bytes::from(data), sample_rate, 1);

        let opts = SttOptions { language, ..Default::default() };
        let transcript = self
            .stt
            .transcribe(&frame, &opts)
            .await
            .map_err(|e| adk_core::AdkError::Tool(format!("transcribe: {e}")))?;

        Ok(serde_json::json!({
            "text": transcript.text,
            "confidence": transcript.confidence
        }))
    }
}

/// Simple base64 decoder (avoids adding a dependency).
fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let input = input.as_bytes();
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &b in input {
        if b == b'=' || b == b'\n' || b == b'\r' {
            continue;
        }
        let val = TABLE
            .iter()
            .position(|&c| c == b)
            .ok_or_else(|| format!("invalid base64 character: {}", b as char))?
            as u32;
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
}
