//! README Realtime Voice Agents snippet validation

use adk_realtime::{openai::OpenAIRealtimeModel, RealtimeAgent, RealtimeModel};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    let model: Arc<dyn RealtimeModel> =
        Arc::new(OpenAIRealtimeModel::new(&api_key, "gpt-4o-realtime-preview-2024-12-17"));

    let _agent = RealtimeAgent::builder("voice_assistant")
        .model(model)
        .instruction("You are a helpful voice assistant.")
        .voice("alloy")
        .server_vad() // Enable voice activity detection
        .build()?;

    println!("✓ Realtime snippet compiles");
    Ok(())
}
