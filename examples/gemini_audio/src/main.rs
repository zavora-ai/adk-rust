//! Gemini Audio — Agentic TTS and STT examples.
//!
//! Demonstrates two agentic scenarios:
//!
//! 1. **TTS Agent** — An agent writes a podcast script, then a tool synthesizes
//!    it to speech using Gemini 3.1 Flash TTS. The audio is saved to a WAV file.
//!
//! 2. **STT Agent** — An agent receives audio (the WAV from scenario 1), a tool
//!    transcribes it using Gemini STT, and the agent summarizes the transcript.
//!
//! # Usage
//!
//! ```bash
//! export GOOGLE_API_KEY=your-key-here
//! cargo run --manifest-path examples/gemini_audio/Cargo.toml
//! ```

use adk_audio::{
    AudioFrame, GeminiStt, GeminiTts, SpeakerConfig, SttOptions, SttProvider, TtsProvider,
    TtsRequest,
};
use adk_core::{Content, Part, SessionId, UserId};
use adk_model::GeminiModel;
use adk_rust::futures::StreamExt;
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

const APP_NAME: &str = "gemini-audio-example";
const MODEL: &str = "gemini-2.5-flash";

fn load_dotenv() {
    let mut dir = std::env::current_dir().ok();
    while let Some(d) = dir {
        let path = d.join(".env");
        if path.is_file() {
            let _ = dotenvy::from_path(path);
            return;
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
}

fn api_key() -> String {
    std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set")
}

/// Write raw PCM16 24kHz mono to a WAV file.
fn write_wav(path: &std::path::Path, pcm: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    let data_len = pcm.len() as u32;
    let file_len = 36 + data_len;
    // RIFF header
    f.write_all(b"RIFF")?;
    f.write_all(&file_len.to_le_bytes())?;
    f.write_all(b"WAVE")?;
    // fmt chunk
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?; // chunk size
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&1u16.to_le_bytes())?; // mono
    f.write_all(&24000u32.to_le_bytes())?; // sample rate
    f.write_all(&48000u32.to_le_bytes())?; // byte rate (24000 * 2)
    f.write_all(&2u16.to_le_bytes())?; // block align
    f.write_all(&16u16.to_le_bytes())?; // bits per sample
    // data chunk
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    f.write_all(pcm)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared state for passing audio between scenarios
// ---------------------------------------------------------------------------

struct SharedAudio {
    wav_path: Option<PathBuf>,
    audio_frame: Option<AudioFrame>,
}

type SharedAudioState = Arc<RwLock<SharedAudio>>;

// ---------------------------------------------------------------------------
// TTS Tool — synthesizes text to speech
// ---------------------------------------------------------------------------

fn make_tts_tool(shared: SharedAudioState) -> Arc<dyn Tool> {
    Arc::new(
        FunctionTool::new(
            "synthesize_speech",
            "Synthesize text to speech using Gemini TTS. Takes 'text' (the script to speak), \
             'voice' (voice name like Kore, Puck, Fenrir), and optional 'style' (e.g. 'cheerful'). \
             Returns the path to the saved WAV file.",
            move |_ctx: Arc<dyn ToolContext>, args: serde_json::Value| {
                let shared = shared.clone();
                async move {
                    let text = args["text"].as_str().unwrap_or("Hello world");
                    let voice = args["voice"].as_str().unwrap_or("Kore");
                    let style = args["style"].as_str().unwrap_or("");

                    println!("  🎙️  [synthesize_speech] voice={voice}, style={style}");
                    println!("  🎙️  text: {}", &text[..text.len().min(80)]);

                    let tts = GeminiTts::from_env().map_err(|e| {
                        adk_core::AdkError::tool(format!("TTS init failed: {e}"))
                    })?;

                    // Prepend style direction if provided
                    let full_text = if style.is_empty() {
                        text.to_string()
                    } else {
                        format!("Say {style}: {text}")
                    };

                    let request = TtsRequest {
                        text: full_text,
                        voice: voice.to_string(),
                        ..Default::default()
                    };

                    let frame = tts.synthesize(&request).await.map_err(|e| {
                        adk_core::AdkError::tool(format!("TTS synthesis failed: {e}"))
                    })?;

                    let duration_secs =
                        frame.data.len() as f64 / (frame.sample_rate as f64 * 2.0);
                    println!(
                        "  🎙️  generated {:.1}s of audio ({} bytes)",
                        duration_secs,
                        frame.data.len()
                    );

                    let wav_path = std::env::temp_dir().join("gemini_tts_output.wav");
                    write_wav(&wav_path, &frame.data).map_err(|e| {
                        adk_core::AdkError::tool(format!("WAV write failed: {e}"))
                    })?;
                    println!("  🎙️  saved to {}", wav_path.display());

                    // Store for STT scenario
                    let mut state = shared.write().await;
                    state.wav_path = Some(wav_path.clone());
                    state.audio_frame = Some(frame);

                    Ok(json!({
                        "status": "success",
                        "wav_path": wav_path.to_string_lossy(),
                        "duration_seconds": duration_secs,
                        "voice": voice,
                    }))
                }
            },
        )
        .with_parameters_schema::<SynthesizeSpeechArgs>(),
    )
}

#[derive(schemars::JsonSchema, serde::Serialize, serde::Deserialize)]
struct SynthesizeSpeechArgs {
    /// The text/script to synthesize to speech.
    text: String,
    /// Voice name (e.g. Kore, Puck, Fenrir, Aoede, Charon).
    voice: Option<String>,
    /// Style direction (e.g. "cheerfully", "in a calm tone", "with excitement").
    style: Option<String>,
}

// ---------------------------------------------------------------------------
// STT Tool — transcribes audio to text
// ---------------------------------------------------------------------------

fn make_stt_tool(shared: SharedAudioState) -> Arc<dyn Tool> {
    Arc::new(
        FunctionTool::new(
            "transcribe_audio",
            "Transcribe audio to text using Gemini STT. Reads the most recently generated \
             audio file. Returns the transcription text.",
            move |_ctx: Arc<dyn ToolContext>, _args: serde_json::Value| {
                let shared = shared.clone();
                async move {
                    let state = shared.read().await;
                    let frame = state.audio_frame.as_ref().ok_or_else(|| {
                        adk_core::AdkError::tool("no audio available to transcribe")
                    })?;

                    println!(
                        "  👂 [transcribe_audio] transcribing {} bytes of audio...",
                        frame.data.len()
                    );

                    let stt = GeminiStt::from_env().map_err(|e| {
                        adk_core::AdkError::tool(format!("STT init failed: {e}"))
                    })?;

                    let transcript = stt
                        .transcribe(frame, &SttOptions::default())
                        .await
                        .map_err(|e| {
                            adk_core::AdkError::tool(format!("STT transcription failed: {e}"))
                        })?;

                    println!("  👂 transcribed: {}", &transcript.text[..transcript.text.len().min(100)]);

                    Ok(json!({
                        "status": "success",
                        "transcript": transcript.text,
                        "confidence": transcript.confidence,
                    }))
                }
            },
        )
        .with_parameters_schema::<TranscribeAudioArgs>(),
    )
}

#[derive(schemars::JsonSchema, serde::Serialize, serde::Deserialize)]
struct TranscribeAudioArgs {
    /// Optional language hint (BCP-47 code like "en", "es", "ja").
    language: Option<String>,
}

// ---------------------------------------------------------------------------
// Runner helpers
// ---------------------------------------------------------------------------

async fn make_runner(agent: Arc<dyn Agent>, session_id: &str) -> anyhow::Result<Runner> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: HashMap::new(),
        })
        .await?;
    Ok(Runner::new(RunnerConfig {
        app_name: APP_NAME.into(),
        agent,
        session_service: sessions,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
    })?)
}

fn separator(title: &str) {
    println!("\n{}", "=".repeat(60));
    println!("  {title}");
    println!("{}\n", "=".repeat(60));
}

async fn run_agent(
    session_id: &str,
    instruction: &str,
    tools: Vec<Arc<dyn Tool>>,
    prompt: &str,
) -> anyhow::Result<()> {
    let model = Arc::new(GeminiModel::new(api_key(), MODEL)?);
    let mut builder = LlmAgentBuilder::new(session_id).instruction(instruction).model(model);
    for tool in tools {
        builder = builder.tool(tool);
    }
    let agent = Arc::new(builder.build()?);
    let runner = make_runner(agent, session_id).await?;

    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new(session_id)?,
            Content::new("user").with_text(prompt),
        )
        .await?;

    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::FunctionCall { name, .. } => println!("  → tool: {name}"),
                    Part::FunctionResponse { function_response, .. } => {
                        println!("  ← {}: done", function_response.name);
                    }
                    Part::Text { text } if !text.trim().is_empty() => print!("{text}"),
                    Part::Thinking { .. } => print!("💭"),
                    _ => {}
                }
            }
        }
    }
    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

async fn scenario_tts(shared: SharedAudioState) -> anyhow::Result<()> {
    separator("Scenario 1: TTS Agent — Write & Speak a Podcast Intro");

    run_agent(
        "tts-agent",
        "You are a podcast producer. When asked to create a podcast intro, \
         write a short engaging script (2-3 sentences), then use the synthesize_speech \
         tool to convert it to audio. Choose a voice that fits the tone. \
         After synthesis, report the result.",
        vec![make_tts_tool(shared)],
        "Create a podcast intro for a tech show called 'Rust & Beyond'. \
         Make it energetic and welcoming.",
    )
    .await
}

async fn scenario_stt(shared: SharedAudioState) -> anyhow::Result<()> {
    separator("Scenario 2: STT Agent — Transcribe & Summarize");

    run_agent(
        "stt-agent",
        "You are a transcription assistant. Use the transcribe_audio tool to \
         transcribe the available audio, then provide a brief summary of what was said.",
        vec![make_stt_tool(shared)],
        "Transcribe the audio that was just recorded and summarize it.",
    )
    .await
}

// ---------------------------------------------------------------------------
// Multi-speaker TTS (bonus — no agent, direct API)
// ---------------------------------------------------------------------------

async fn scenario_multi_speaker() -> anyhow::Result<()> {
    separator("Scenario 3: Multi-Speaker TTS (Direct API)");

    let tts = GeminiTts::from_env()?
        .with_speakers(vec![
            SpeakerConfig::new("Host", "Fenrir"),
            SpeakerConfig::new("Guest", "Kore"),
        ]);

    let script = r#"Host: Welcome back to Rust & Beyond! Today we have a special guest.
Guest: Thanks for having me! I'm excited to talk about ADK-Rust.
Host: Let's dive right in. What makes it different?"#;

    println!("  📝 Script:\n{script}\n");

    let request = TtsRequest { text: script.into(), ..Default::default() };
    let frame = tts.synthesize(&request).await?;

    let duration = frame.data.len() as f64 / (frame.sample_rate as f64 * 2.0);
    let path = std::env::temp_dir().join("gemini_multi_speaker.wav");
    write_wav(&path, &frame.data)?;

    println!("  🎙️  Multi-speaker audio: {:.1}s, saved to {}", duration, path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv();

    println!("Gemini Audio — Agentic TTS & STT Example");
    println!("=========================================");
    println!("Model: {MODEL}\n");

    let shared: SharedAudioState = Arc::new(RwLock::new(SharedAudio {
        wav_path: None,
        audio_frame: None,
    }));

    // Scenario 1: Agent writes script → TTS tool speaks it
    if let Err(e) = scenario_tts(shared.clone()).await {
        eprintln!("✗ Scenario 1 (TTS) failed: {e:#}");
    }

    // Scenario 2: Agent uses STT tool to transcribe the audio from scenario 1
    if shared.read().await.audio_frame.is_some() {
        if let Err(e) = scenario_stt(shared.clone()).await {
            eprintln!("✗ Scenario 2 (STT) failed: {e:#}");
        }
    } else {
        println!("\n⚠ Skipping STT scenario — no audio from TTS scenario");
    }

    // Scenario 3: Multi-speaker TTS (direct, no agent)
    if let Err(e) = scenario_multi_speaker().await {
        eprintln!("✗ Scenario 3 (Multi-speaker) failed: {e:#}");
    }

    println!("\nDone.");
    Ok(())
}
