//! Transcription Agent — transcribe audio files and produce structured summaries.
//!
//! Demonstrates an LlmAgent that:
//! 1. Loads a sample WAV file from disk
//! 2. Transcribes the audio via `TranscribeTool` (Whisper API)
//! 3. Uses LLM reasoning to produce a structured text summary
//!
//! Requires `OPENAI_API_KEY` environment variable.
//!
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run -p audio-examples --example transcription_agent --features agents,openai
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_audio::{TranscribeTool, WhisperApiStt};
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

/// Read a WAV file and return its raw PCM16 bytes.
fn read_wav_pcm16(path: &str) -> anyhow::Result<(Vec<u8>, u32)> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let sample_rate = spec.sample_rate;

    let mut pcm_bytes = Vec::new();
    match spec.sample_format {
        hound::SampleFormat::Int => {
            for sample in reader.samples::<i16>() {
                let s = sample?;
                pcm_bytes.extend_from_slice(&s.to_le_bytes());
            }
        }
        hound::SampleFormat::Float => {
            for sample in reader.samples::<f32>() {
                let s = sample?;
                let i = (s * 32767.0).clamp(-32768.0, 32767.0) as i16;
                pcm_bytes.extend_from_slice(&i.to_le_bytes());
            }
        }
    }
    Ok((pcm_bytes, sample_rate))
}

/// Simple base64 encoder for PCM16 data.
fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let api_key = require_env("OPENAI_API_KEY")?;

    // --- Load sample WAV file ---
    let wav_path = "examples/audio/samples/openai_tts_alloy.wav";
    println!("=== Transcription Agent ===\n");
    println!("Loading audio from: {wav_path}");

    let (pcm_data, sample_rate) = read_wav_pcm16(wav_path)?;
    let audio_b64 = base64_encode(&pcm_data);
    println!(
        "  PCM16 data: {} bytes, {}Hz, base64 length: {}\n",
        pcm_data.len(),
        sample_rate,
        audio_b64.len()
    );

    // --- Provider ---
    let stt = Arc::new(WhisperApiStt::from_env()?);

    // --- Tool ---
    let transcribe_tool = TranscribeTool::new(stt);

    // --- LLM model ---
    let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-4o-mini"))?;

    // --- Agent ---
    let agent = LlmAgentBuilder::new("transcription_agent")
        .model(Arc::new(model))
        .instruction(
            "You are a transcription agent. When given audio data, you MUST:\n\
             1. Call the `transcribe` tool with the provided audio_data and sample_rate.\n\
             2. Once you receive the transcription, produce a structured summary with:\n\
                - **Transcript**: The full transcribed text\n\
                - **Word Count**: Number of words in the transcript\n\
                - **Language**: Detected or assumed language\n\
                - **Summary**: A one-sentence summary of the content\n\
                - **Key Topics**: A comma-separated list of main topics mentioned\n\n\
             Always call the transcribe tool first before producing the summary.",
        )
        .tool(Arc::new(transcribe_tool))
        .build()?;

    // --- Session + Runner ---
    let session_service = Arc::new(InMemorySessionService::new());
    let session = session_service
        .create(CreateRequest {
            app_name: "transcription_agent".to_string(),
            user_id: "user_1".to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;
    let session_id = session.id().to_string();

    let runner = Runner::new(RunnerConfig {
        app_name: "transcription_agent".to_string(),
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
    println!("Asking the agent to transcribe and summarize the audio...\n");

    let prompt = Content::new("user").with_text(format!(
        "Please transcribe the following audio and produce a structured summary.\n\
         Audio data (base64-encoded PCM16, {sample_rate}Hz):\n{audio_b64}"
    ));

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
