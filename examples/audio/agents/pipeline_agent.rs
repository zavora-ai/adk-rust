//! Audio Pipeline Agent — LlmAgent text generation fed into an AudioPipelineBuilder.
//!
//! Demonstrates combining an LlmAgent with `AudioPipelineBuilder` for end-to-end
//! voice processing:
//! 1. An LlmAgent generates narration text via LLM reasoning
//! 2. The generated text is fed into an `AudioPipelineBuilder` TTS pipeline (OpenAI TTS)
//! 3. Pipeline output is collected (audio frames + metrics)
//! 4. `PipelineMetrics` are displayed (TTS latency, total audio duration)
//! 5. Graceful shutdown via `PipelineControl::Stop`
//!
//! Requires `OPENAI_API_KEY` environment variable.
//!
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run -p audio-examples --example pipeline_agent --features agents,openai
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_audio::{AudioPipelineBuilder, OpenAiTts, PipelineControl, PipelineInput, PipelineOutput};
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

    println!("=== Audio Pipeline Agent ===\n");

    // --- Step 1: Create the LlmAgent to generate narration text ---
    let model = OpenAIClient::new(OpenAIConfig::new(&api_key, "gpt-4o-mini"))?;

    let agent = LlmAgentBuilder::new("narration_generator")
        .model(Arc::new(model))
        .instruction(
            "You are a narration writer. When asked to write narration, produce a short \
             narration script of 2-3 sentences. Output ONLY the narration text, no \
             formatting, no markdown, no extra commentary. Keep it concise and engaging.",
        )
        .build()?;

    // --- Session + Runner ---
    let session_service = Arc::new(InMemorySessionService::new());
    let session = session_service
        .create(CreateRequest {
            app_name: "pipeline_agent".to_string(),
            user_id: "user_1".to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;
    let session_id = session.id().to_string();

    let runner = Runner::new(RunnerConfig {
        app_name: "pipeline_agent".to_string(),
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

    // --- Step 2: Run the agent to generate narration text ---
    println!("Asking the agent to generate narration text...\n");

    let prompt = Content::new("user")
        .with_text("Write a short narration about the dawn of artificial intelligence.");

    let mut stream = runner.run("user_1".to_string(), session_id, prompt).await?;

    let mut generated_text = String::new();
    while let Some(event) = stream.next().await {
        match event {
            Ok(e) => {
                if let Some(content) = &e.llm_response.content {
                    for part in &content.parts {
                        if let Part::Text { text } = part {
                            generated_text.push_str(text);
                        }
                    }
                }
            }
            Err(e) => eprintln!("Agent error: {e}"),
        }
    }

    let generated_text = generated_text.trim().to_string();
    println!("Agent generated text:\n  \"{generated_text}\"\n");

    if generated_text.is_empty() {
        anyhow::bail!("Agent produced no text — cannot feed pipeline");
    }

    // --- Step 3: Build the TTS pipeline with real OpenAI TTS ---
    println!("Building TTS pipeline with OpenAI TTS...\n");

    let tts = Arc::new(OpenAiTts::from_env()?);
    let mut handle = AudioPipelineBuilder::new().tts(tts).buffer_size(16).build_tts()?;

    println!("Pipeline started.\n");

    // --- Step 4: Feed the generated text into the pipeline ---
    println!("Sending generated text to pipeline...");
    handle.input_tx.send(PipelineInput::Text(generated_text)).await?;

    // --- Step 5: Collect pipeline output (audio frames + metrics) ---
    let mut audio_frame_count = 0u32;
    let mut total_audio_bytes = 0usize;

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                println!("\nTimeout waiting for pipeline output.");
                break;
            }
            output = handle.output_rx.recv() => {
                match output {
                    Some(PipelineOutput::Audio(frame)) => {
                        audio_frame_count += 1;
                        total_audio_bytes += frame.data.len();
                        println!(
                            "  ← Audio frame {audio_frame_count}: {}ms, {}Hz, {} bytes",
                            frame.duration_ms, frame.sample_rate, frame.data.len()
                        );
                    }
                    Some(PipelineOutput::Metrics(m)) => {
                        println!(
                            "  ← Metrics: tts_latency={:.1}ms, total_audio={}ms",
                            m.tts_latency_ms, m.total_audio_ms
                        );
                        // Metrics signal the end of processing for this input
                        break;
                    }
                    Some(PipelineOutput::AgentText(text)) => {
                        println!("  ← Agent text: \"{text}\"");
                    }
                    Some(PipelineOutput::Transcript(t)) => {
                        println!("  ← Transcript: \"{}\"", t.text);
                    }
                    None => {
                        println!("Pipeline output channel closed.");
                        break;
                    }
                }
            }
        }
    }

    // --- Step 6: Display accumulated pipeline metrics ---
    let metrics = handle.metrics.read().await;
    println!("\n--- Pipeline Metrics ---");
    println!("  TTS latency:   {:.1}ms", metrics.tts_latency_ms);
    println!("  Total audio:   {}ms", metrics.total_audio_ms);
    println!("  Audio frames:  {audio_frame_count}");
    println!("  Audio bytes:   {total_audio_bytes}");
    drop(metrics);

    // --- Step 7: Graceful shutdown ---
    println!("\nSending graceful shutdown...");
    handle.input_tx.send(PipelineInput::Control(PipelineControl::Stop)).await?;
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    println!("\n=== Done ===");
    Ok(())
}
