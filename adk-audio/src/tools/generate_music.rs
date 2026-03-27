//! GenerateMusicTool — generate music via a configured MusicProvider.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::traits::{MusicProvider, MusicRequest};

/// Tool that generates music from a text prompt.
///
/// Accepts JSON `{prompt, duration_secs, genre?}` and returns `{duration_ms}`.
pub struct GenerateMusicTool {
    music: Arc<dyn MusicProvider>,
}

impl GenerateMusicTool {
    /// Create a new `GenerateMusicTool` with the given music provider.
    pub fn new(music: Arc<dyn MusicProvider>) -> Self {
        Self { music }
    }
}

#[async_trait]
impl adk_core::Tool for GenerateMusicTool {
    fn name(&self) -> &str {
        "generate_music"
    }

    fn description(&self) -> &str {
        "Generate music or ambient audio from a text prompt"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": { "type": "string", "description": "Text description of desired music" },
                "duration_secs": { "type": "integer", "description": "Duration in seconds" },
                "genre": { "type": "string", "description": "Genre hint (optional)" }
            },
            "required": ["prompt", "duration_secs"]
        }))
    }

    async fn execute(
        &self,
        _ctx: Arc<dyn adk_core::ToolContext>,
        args: Value,
    ) -> adk_core::Result<Value> {
        let prompt = args["prompt"].as_str().unwrap_or_default().to_string();
        let duration_secs = args["duration_secs"].as_u64().unwrap_or(10) as u32;
        let genre = args["genre"].as_str().map(String::from);

        let request = MusicRequest { prompt, duration_secs, genre, ..Default::default() };
        let frame = self
            .music
            .generate(&request)
            .await
            .map_err(|e| adk_core::AdkError::tool(format!("generate_music: {e}")))?;

        Ok(serde_json::json!({
            "duration_ms": frame.duration_ms
        }))
    }
}
