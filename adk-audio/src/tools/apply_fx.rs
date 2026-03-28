//! ApplyFxTool — apply an FX chain to audio.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::traits::{AudioProcessor, FxChain};

/// Tool that applies a named FX chain to audio data.
///
/// Accepts JSON referencing audio data and an FX chain name.
pub struct ApplyFxTool {
    chains: HashMap<String, FxChain>,
}

impl ApplyFxTool {
    /// Create a new `ApplyFxTool` with the given named FX chains.
    pub fn new(chains: HashMap<String, FxChain>) -> Self {
        Self { chains }
    }
}

#[async_trait]
impl adk_core::Tool for ApplyFxTool {
    fn name(&self) -> &str {
        "apply_fx"
    }

    fn description(&self) -> &str {
        "Apply audio effects chain to audio data"
    }

    fn parameters_schema(&self) -> Option<Value> {
        let chain_names: Vec<&str> = self.chains.keys().map(|s| s.as_str()).collect();
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "audio_data": { "type": "string", "description": "Base64-encoded PCM16 audio data" },
                "sample_rate": { "type": "integer", "description": "Sample rate in Hz (default 16000)" },
                "chain": { "type": "string", "description": "FX chain name", "enum": chain_names }
            },
            "required": ["audio_data", "chain"]
        }))
    }

    async fn execute(
        &self,
        _ctx: Arc<dyn adk_core::ToolContext>,
        args: Value,
    ) -> adk_core::Result<Value> {
        let chain_name = args["chain"].as_str().unwrap_or_default();
        let chain = self.chains.get(chain_name).ok_or_else(|| {
            adk_core::AdkError::tool(format!("apply_fx: unknown chain '{chain_name}'"))
        })?;

        let audio_b64 = args["audio_data"].as_str().unwrap_or_default();
        let sample_rate = args["sample_rate"].as_u64().unwrap_or(16000) as u32;

        // Decode base64
        let data = base64_decode(audio_b64)
            .map_err(|e| adk_core::AdkError::tool(format!("apply_fx: invalid base64: {e}")))?;
        let frame = crate::frame::AudioFrame::new(bytes::Bytes::from(data), sample_rate, 1);

        let processed = chain
            .process(&frame)
            .await
            .map_err(|e| adk_core::AdkError::tool(format!("apply_fx: {e}")))?;

        Ok(serde_json::json!({
            "duration_ms": processed.duration_ms,
            "sample_rate": processed.sample_rate
        }))
    }
}

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
