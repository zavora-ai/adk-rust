//! ADK-Rust Podcast Episode Generator.
//!
//! Generates a full podcast episode about ADK-Rust using Gemini TTS
//! with two hosts: James (Fenrir) and Ada (Kore).

use adk_audio::{GeminiTts, SpeakerConfig, TtsProvider, TtsRequest};
use std::path::Path;

/// Write raw PCM16 24kHz mono to a WAV file.
fn write_wav(path: &Path, pcm: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    let data_len = pcm.len() as u32;
    let file_len = 36 + data_len;
    f.write_all(b"RIFF")?;
    f.write_all(&file_len.to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&24000u32.to_le_bytes())?;
    f.write_all(&48000u32.to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    f.write_all(pcm)?;
    Ok(())
}

const PODCAST_SCRIPT: &str = r#"James: [excited] Welcome to Rust and Beyond, the podcast where we explore the cutting edge of AI agent development! I'm James.
Ada: And I'm Ada! Today we're diving into ADK-Rust, the open-source Rust framework for building AI agents. James, what makes this framework special?
James: Great question, Ada. ADK-Rust is a production-ready framework that lets you build AI agents in Rust. It's model-agnostic, meaning you can use Gemini, OpenAI, Anthropic, or even local models through mistral.rs. Everything is type-safe and fully async.
Ada: That's huge. So developers aren't locked into one provider. What about the architecture?
James: The architecture is modular. At the core, you have the Agent trait, the Tool trait, and the LLM trait. You compose agents using patterns like Sequential, Parallel, Loop, and Graph workflows. Each agent can have its own tools, instructions, and sub-agents.
Ada: [curious] And I heard there's a graph-based workflow system?
James: Yes! The adk-graph crate gives you a Pregel-style graph executor with checkpoints, durable resume, and human-in-the-loop interrupts. You can build complex multi-step workflows that survive crashes and resume from the last checkpoint.
Ada: That's enterprise-grade reliability. What about real-time voice?
James: ADK-Rust has full real-time audio support through adk-realtime. You can build voice agents using OpenAI's Realtime API, Gemini Live, or even bridge through LiveKit for WebRTC. And now with adk-audio, we have Gemini-powered text-to-speech and speech-to-text built right in.
Ada: [amazed] Wait, so this podcast was actually generated using ADK-Rust?
James: [laughs] Exactly! This episode was created using the Gemini 3.1 Flash TTS model through adk-audio. Two speakers, natural voices, all from a Rust script.
Ada: That's incredible. So what's the best way for developers to get started?
James: Just run cargo install cargo-adk, then cargo adk new my-agent. You'll have a working agent in seconds. The framework handles sessions, memory, tools, and streaming out of the box. Check out the playground at playground.adk-rust.com to try it without installing anything.
Ada: [enthusiastic] Amazing. And it's all open source on GitHub at zavora-ai/adk-rust. Thanks for listening everyone, and happy building!
James: See you next time on Rust and Beyond!"#;

pub async fn generate_podcast(output_path: &Path) -> anyhow::Result<()> {
    println!("🎙️  Generating ADK-Rust podcast episode...\n");
    println!("   Hosts: James (Fenrir) & Ada (Kore)");
    println!("   Model: gemini-3.1-flash-tts-preview\n");

    let tts = GeminiTts::from_env()?.with_speakers(vec![
        SpeakerConfig::new("James", "Fenrir"),
        SpeakerConfig::new("Ada", "Kore"),
    ]);

    let request = TtsRequest { text: PODCAST_SCRIPT.into(), ..Default::default() };

    // Retry up to 3 times — the Gemini TTS API can be slow for long scripts
    // and may drop the connection on the first attempt.
    let max_retries = 3;
    let mut last_err = None;
    for attempt in 1..=max_retries {
        if attempt > 1 {
            let delay = std::time::Duration::from_secs(5 * attempt as u64);
            println!("   ⏳ Retrying in {}s (attempt {attempt}/{max_retries})...", delay.as_secs());
            tokio::time::sleep(delay).await;
        }
        println!("   Synthesizing... (this may take 60-120 seconds for a full episode)");

        match tts.synthesize(&request).await {
            Ok(frame) => {
                let duration = frame.data.len() as f64 / (frame.sample_rate as f64 * 2.0);
                write_wav(output_path, &frame.data)?;

                println!("\n   ✅ Episode generated!");
                println!("   Duration: {:.1}s", duration);
                println!("   Output: {}", output_path.display());
                println!("   Size: {:.1} KB", frame.data.len() as f64 / 1024.0);
                return Ok(());
            }
            Err(e) => {
                eprintln!("   ⚠ Attempt {attempt} failed: {e}");
                last_err = Some(e);
            }
        }
    }

    Err(anyhow::anyhow!(
        "podcast generation failed after {max_retries} attempts: {}",
        last_err.map(|e| e.to_string()).unwrap_or_default()
    ))
}
