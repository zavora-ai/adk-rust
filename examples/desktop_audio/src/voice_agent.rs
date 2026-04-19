//! Full voice agent: Mic → VAD → GeminiStt → LlmAgent → GeminiTts → Speaker.
//!
//! A natural conversational voice agent. You speak, the agent listens,
//! thinks, and responds with synthesized speech through your speaker.
//!
//! Requires `GEMINI_API_KEY` or `GOOGLE_API_KEY` environment variable.
//! Uses real Gemini cloud providers for STT and TTS — no mocks.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_audio::{
    AudioCapture, AudioFrame, AudioPlayback, CaptureConfig, GeminiStt, GeminiTts, SttOptions,
    SttProvider, TtsProvider, TtsRequest, VadProcessor, merge_frames,
};
use adk_core::{Content, Part, SessionId, UserId};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use desktop_audio_example::{MockVad, setup_tracing};
use futures::StreamExt;

const APP_NAME: &str = "desktop-voice-agent";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    setup_tracing();

    println!("=== Voice Agent with Real Gemini STT/TTS ===\n");

    // Verify API key
    let api_key = std::env::var("GEMINI_API_KEY")
        .or_else(|_| std::env::var("GOOGLE_API_KEY"))
        .map_err(|_| {
            anyhow::anyhow!(
                "GEMINI_API_KEY or GOOGLE_API_KEY not set.\n\
                 Copy .env.example to .env and add your key.\n\
                 Get a key at: https://aistudio.google.com/apikey"
            )
        })?;

    // Real Gemini providers
    let stt = GeminiStt::from_env()?;
    let tts = GeminiTts::from_env()?;
    let vad: Arc<dyn VadProcessor> = Arc::new(MockVad { threshold: 500 });

    println!("✅ Gemini STT initialized (gemini-3-flash-preview)");
    println!("✅ Gemini TTS initialized (gemini-3.1-flash-tts-preview)");

    // LlmAgent with Gemini
    let model = Arc::new(adk_model::GeminiModel::new(&api_key, "gemini-2.5-flash")?);
    let agent: Arc<dyn adk_core::Agent> = Arc::new(
        adk_agent::LlmAgentBuilder::new("voice_assistant")
            .model(model)
            .instruction(
                "You are a helpful, friendly voice assistant. \
                 Keep responses concise and conversational — 1 to 2 sentences max. \
                 Respond naturally as if having a spoken conversation.",
            )
            .build()?,
    );
    println!("✅ LlmAgent initialized (gemini-2.5-flash)");

    // Session service + runner
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some("voice-session".into()),
            state: HashMap::new(),
        })
        .await?;

    let runner = adk_runner::Runner::new(adk_runner::RunnerConfig {
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
        intra_compaction_config: None,
        intra_compaction_summarizer: None,
    })?;
    println!("✅ Runner initialized\n");

    // Audio devices
    let input_devices = AudioCapture::list_input_devices()?;
    let input_device =
        input_devices.first().ok_or_else(|| anyhow::anyhow!("no input devices found"))?;
    let output_devices = AudioPlayback::list_output_devices()?;
    let output_device =
        output_devices.first().ok_or_else(|| anyhow::anyhow!("no output devices found"))?;

    println!("🎤 Input:  {}", input_device.name());
    println!("🔊 Output: {}", output_device.name());

    // Start capture
    let config = CaptureConfig::default();
    let mut capture = AudioCapture::new();
    let mut stream = capture.start_capture(input_device.id(), &config)?;
    let mut playback = AudioPlayback::new();
    let out_id = output_device.id().to_string();

    println!("\n💬 Speak into your microphone. The agent will respond.\n");
    println!("   (Running for 60 seconds — Ctrl+C to stop early)\n");

    let start = std::time::Instant::now();
    let max_duration = Duration::from_secs(60);

    // Conversation state
    let mut speech_buffer: Vec<AudioFrame> = Vec::new();
    let mut is_speaking = false;
    let mut consecutive_silence: u32 = 0;
    let silence_flush_ms: u32 = 600;
    let mut turn_count = 0u32;

    loop {
        if start.elapsed() >= max_duration {
            println!("\n⏰ Time's up!");
            break;
        }

        // Receive next frame with timeout
        let frame = tokio::select! {
            f = stream.recv() => match f {
                Some(frame) => frame,
                None => break,
            },
            _ = tokio::time::sleep(Duration::from_millis(100)) => continue,
        };

        let speech = vad.is_speech(&frame);

        if speech {
            if !is_speaking {
                is_speaking = true;
                consecutive_silence = 0;
                speech_buffer.clear();
                println!("🎙️  Listening...");
            }
            speech_buffer.push(frame);
            consecutive_silence = 0;
        } else if is_speaking {
            speech_buffer.push(frame.clone());
            consecutive_silence += frame.duration_ms;

            if consecutive_silence >= silence_flush_ms && !speech_buffer.is_empty() {
                is_speaking = false;
                turn_count += 1;

                let merged = merge_frames(&speech_buffer);
                speech_buffer.clear();

                println!("   ({}ms of audio captured)", merged.duration_ms);

                // 1. STT — transcribe the speech
                print!("📝 Transcribing... ");
                let transcript = match stt.transcribe(&merged, &SttOptions::default()).await {
                    Ok(t) => t,
                    Err(e) => {
                        println!("error: {e}");
                        continue;
                    }
                };

                if transcript.text.trim().is_empty() {
                    println!("(empty transcript, skipping)");
                    continue;
                }
                println!("\"{}\"", transcript.text);

                // 2. Agent — get a conversational response
                print!("🤔 Thinking... ");
                let user_content = Content::new("user").with_text(&transcript.text);

                let mut event_stream = match runner
                    .run(
                        UserId::new("user")?,
                        SessionId::new("voice-session")?,
                        user_content,
                    )
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        println!("error: {e}");
                        continue;
                    }
                };

                // Collect agent response text from the event stream
                let mut agent_text = String::new();
                while let Some(result) = event_stream.next().await {
                    match result {
                        Ok(event) => {
                            if event.llm_response.partial {
                                continue; // Skip streaming chunks
                            }
                            if let Some(content) = &event.llm_response.content {
                                for part in &content.parts {
                                    if let Part::Text { text: t } = part {
                                        if !t.trim().is_empty() {
                                            agent_text.push_str(t);
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("agent error: {e}");
                            break;
                        }
                    }
                }

                if agent_text.trim().is_empty() {
                    println!("(no response)");
                    continue;
                }
                println!("\n🤖 Agent: \"{agent_text}\"");

                // 3. TTS — synthesize the response and play it
                print!("🔊 Speaking... ");
                let tts_request = TtsRequest { text: agent_text, ..Default::default() };
                match tts.synthesize(&tts_request).await {
                    Ok(audio_frame) => {
                        println!("({}ms audio at {}Hz)", audio_frame.duration_ms, audio_frame.sample_rate);
                        if let Err(e) = playback.play(&out_id, &audio_frame).await {
                            println!("playback error: {e}");
                        }
                        // Wait for playback to finish
                        let wait_ms = audio_frame.duration_ms as u64 + 300;
                        tokio::time::sleep(Duration::from_millis(wait_ms)).await;
                    }
                    Err(e) => {
                        println!("TTS error: {e}");
                    }
                }

                println!();
            }
        }
    }

    // Cleanup
    capture.stop_capture();
    playback.stop();

    println!("--- Voice agent complete ({turn_count} turns) ---");
    Ok(())
}
