//! Voice Assistant Agent — speak and transcribe audio with LLM reasoning.
//!
//! Demonstrates an LlmAgent that:
//! 1. Synthesizes speech via `SpeakTool` (OpenAI TTS)
//! 2. Transcribes audio via `TranscribeTool` (Whisper API)
//! 3. Uses LLM reasoning to decide when to speak or transcribe
//!
//! Requires `OPENAI_API_KEY` environment variable.
//!
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run -p audio-examples --example voice_assistant --features agents,openai
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_audio::{OpenAiTts, SpeakTool, TranscribeTool, WhisperApiStt};
use adk_core::{Content, Part};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;

/// Check for a required environment variable, returning a clear error if missing.
fn require_env(key: &str) -> anyhow::Result<String> {
    std::env::var(key).map_err(|_| {
        anyhow::anyhow!(
            "Missing required environment variable: {key}\n\
             Set it in your .env file or export it:\n  export {key}=your-key-here"
        )
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let api_key = require_env("OPENAI_API_KEY")?;

    println!("=== Voice Assistant Agent ===\n");

    // --- Providers ---
    let tts = Arc::new(OpenAiTts::from_env()?);
    let stt = Arc::new(WhisperApiStt::from_env()?);

    // --- Tools ---
    let speak_tool = SpeakTool::new(tts, "alloy");
    let transcribe_tool = TranscribeTool::new(stt);

    // --- LLM model ---
    let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-4o-mini"))?;

    // --- Agent ---
    let agent = LlmAgentBuilder::new("voice_assistant")
        .model(Arc::new(model))
        .instruction(
            "You are a voice assistant agent with two audio tools:\n\
             - `speak`: Synthesize text to speech audio (params: text, voice, emotion)\n\
             - `transcribe`: Transcribe audio data to text (params: audio_data, sample_rate)\n\n\
             When the user asks you to say something, use the `speak` tool.\n\
             When the user provides audio data, use the `transcribe` tool.\n\
             Always explain what you did after each tool call.",
        )
        .tool(Arc::new(speak_tool))
        .tool(Arc::new(transcribe_tool))
        .build()?;

    // --- Session + Runner ---
    let session_service = Arc::new(InMemorySessionService::new());
    let session = session_service
        .create(CreateRequest {
            app_name: "voice_assistant".to_string(),
            user_id: "user_1".to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;
    let session_id = session.id().to_string();

    let runner = Runner::new(RunnerConfig {
        app_name: "voice_assistant".to_string(),
        agent: Arc::new(agent),
        session_service,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
    })?;

    // --- Run the agent: ask it to speak ---
    println!("Asking the agent to speak a greeting...\n");

    let prompt = Content::new("user").with_text(
        "Please say 'Hello! Welcome to the ADK voice assistant demo.' in a friendly tone.",
    );

    let mut stream = runner.run("user_1".to_string(), session_id, prompt).await?;

    while let Some(event) = stream.next().await {
        match event {
            Ok(e) => {
                if let Some(content) = &e.llm_response.content {
                    for part in &content.parts {
                        match part {
                            Part::Text { text } => print!("{text}"),
                            Part::FunctionCall { name, .. } => {
                                println!("\n[Tool call: {name}]");
                            }
                            Part::FunctionResponse { function_response, .. } => {
                                println!(
                                    "[Tool response ({name}): {resp}]",
                                    name = function_response.name,
                                    resp = function_response.response
                                );
                            }
                            _ => {}
                        }
                    }
                }
            }
            Err(e) => eprintln!("Error: {e}"),
        }
    }

    println!("\n\n=== Done ===");
    Ok(())
}
