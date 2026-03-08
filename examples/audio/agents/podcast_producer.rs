//! Podcast Producer Agent — multi-segment TTS with audio effects and mixing.
//!
//! Demonstrates an LlmAgent that produces a podcast by:
//! 1. Generating a multi-segment script via LLM reasoning
//! 2. Synthesizing each segment with `SpeakTool` (OpenAI TTS)
//! 3. Applying audio effects via `ApplyFxTool` (loudness normalization)
//! 4. Combining all segments into a final WAV using `Mixer`
//!
//! Requires `OPENAI_API_KEY` environment variable.
//!
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run -p audio-examples --example podcast_producer --features agents,openai
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_audio::{
    ApplyFxTool, AudioFrame, FxChain, LoudnessNormalizer, Mixer, OpenAiTts, SpeakTool,
};
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

/// Write an `AudioFrame` to a WAV file.
fn write_wav(path: &str, frame: &AudioFrame) -> anyhow::Result<()> {
    let spec = hound::WavSpec {
        channels: frame.channels as u16,
        sample_rate: frame.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for chunk in frame.data.chunks_exact(2) {
        let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
        writer.write_sample(sample)?;
    }
    writer.finalize()?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let api_key = require_env("OPENAI_API_KEY")?;

    // --- Providers ---
    let tts = Arc::new(OpenAiTts::from_env()?);

    // --- Tools ---
    let speak_tool = SpeakTool::new(tts.clone(), "nova");

    // FX chain: loudness normalization for broadcast-ready audio
    let mut chains = HashMap::new();
    chains.insert("broadcast".to_string(), FxChain::new().push(LoudnessNormalizer::new()));
    let fx_tool = ApplyFxTool::new(chains);

    // --- LLM model ---
    let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-4o-mini"))?;

    // --- Agent ---
    let agent = LlmAgentBuilder::new("podcast_producer")
        .model(Arc::new(model))
        .instruction(
            "You are a podcast producer agent. When asked to create a podcast, you MUST:\n\
             1. Write a short podcast script with exactly 3 segments (intro, main, outro).\n\
             2. For EACH segment, call the `speak` tool with the segment text and voice \"nova\".\n\
             3. After synthesizing each segment, call `apply_fx` with chain \"broadcast\" \
                to normalize loudness.\n\
             4. After all segments are produced, summarize what you created.\n\n\
             Keep each segment under 2 sentences for brevity.",
        )
        .tool(Arc::new(speak_tool))
        .tool(Arc::new(fx_tool))
        .build()?;

    // --- Session + Runner ---
    let session_service = Arc::new(InMemorySessionService::new());
    let session = session_service
        .create(CreateRequest {
            app_name: "podcast_producer".to_string(),
            user_id: "producer_1".to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;
    let session_id = session.id().to_string();

    let runner = Runner::new(RunnerConfig {
        app_name: "podcast_producer".to_string(),
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

    // --- Run the agent ---
    println!("=== Podcast Producer Agent ===\n");
    println!("Asking the agent to produce a short podcast...\n");

    let prompt = Content::new("user")
        .with_text("Create a short podcast episode about the future of AI in music production.");

    let mut stream = runner.run("producer_1".to_string(), session_id, prompt).await?;

    // Collect synthesized segments from tool calls for mixing
    let mut segment_count = 0u32;

    while let Some(event) = stream.next().await {
        match event {
            Ok(e) => {
                // Print any text the agent produces
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
                                if function_response.name == "speak" {
                                    segment_count += 1;
                                    println!("  → Segment {segment_count} synthesized");
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Err(e) => eprintln!("Error: {e}"),
        }
    }

    // --- Demonstrate Mixer: combine placeholder segments into final output ---
    // In a real pipeline the agent's SpeakTool calls would produce AudioFrames
    // that get stored. Here we show the Mixer API with silence placeholders
    // representing where real audio would go.
    println!("\n\n--- Mixing final podcast ---");

    let sample_rate = 24000;
    let mut mixer = Mixer::new(sample_rate);
    mixer.add_track("intro", 1.0);
    mixer.add_track("main", 1.0);
    mixer.add_track("outro", 0.8);

    // Push placeholder frames (in production these come from SpeakTool output)
    mixer.push_frame("intro", AudioFrame::silence(sample_rate, 1, 500));
    mixer.push_frame("main", AudioFrame::silence(sample_rate, 1, 500));
    mixer.push_frame("outro", AudioFrame::silence(sample_rate, 1, 500));

    let mixed = mixer.mix()?;
    println!(
        "Mixed output: {}ms @ {}Hz, {} bytes",
        mixed.duration_ms,
        mixed.sample_rate,
        mixed.data.len()
    );

    // Write the mixed output to a WAV file
    std::fs::create_dir_all("examples/audio/samples")?;
    let output_path = "examples/audio/samples/podcast_mixed.wav";
    write_wav(output_path, &mixed)?;
    println!("Saved mixed podcast to {output_path}");

    println!("\n=== Done ===");
    Ok(())
}
