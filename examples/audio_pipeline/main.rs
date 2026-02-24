//! Audio pipeline example — build and run a TTS pipeline with the builder API.
//!
//! Demonstrates:
//! - AudioPipelineBuilder for constructing pipelines
//! - Sending text input and receiving audio output
//! - Pipeline metrics collection
//! - Graceful shutdown via PipelineControl::Stop
//!
//! # Run
//!
//! ```bash
//! cargo run --example audio_pipeline --features audio
//! ```

use std::pin::Pin;
use std::sync::Arc;

use adk_audio::{
    AudioFrame, AudioPipelineBuilder, PipelineControl, PipelineInput, PipelineOutput, TtsProvider,
    TtsRequest,
};
use anyhow::Result;

/// Mock TTS provider that generates silent frames.
struct DemoTtsProvider;

#[async_trait::async_trait]
impl TtsProvider for DemoTtsProvider {
    async fn synthesize(&self, request: &TtsRequest) -> adk_audio::AudioResult<AudioFrame> {
        let words = request.text.split_whitespace().count();
        let duration_ms = (words as u32 * 350).max(50);
        Ok(AudioFrame::silence(24000, 1, duration_ms))
    }

    async fn synthesize_stream(
        &self,
        request: &TtsRequest,
    ) -> adk_audio::AudioResult<
        Pin<Box<dyn futures::Stream<Item = adk_audio::AudioResult<AudioFrame>> + Send>>,
    > {
        let frame = self.synthesize(request).await?;
        Ok(Box::pin(futures::stream::once(async { Ok(frame) })))
    }

    fn voice_catalog(&self) -> &[adk_audio::Voice] {
        &[]
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    println!("=== adk-audio: Pipeline Example ===\n");

    // Build a TTS pipeline
    let tts: Arc<dyn TtsProvider> = Arc::new(DemoTtsProvider);
    let mut handle = AudioPipelineBuilder::new().tts(tts).buffer_size(16).build_tts()?;

    println!("Pipeline started.\n");

    // Send text inputs
    let texts = [
        "Welcome to the audio pipeline demo.",
        "This text is converted to speech through the pipeline.",
        "Each message produces an audio frame on the output channel.",
    ];

    for (i, text) in texts.iter().enumerate() {
        println!("→ Sending: \"{text}\"");
        handle.input_tx.send(PipelineInput::Text(text.to_string())).await?;

        // Collect output (with a short timeout so we don't block forever)
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(500);
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                output = handle.output_rx.recv() => {
                    match output {
                        Some(PipelineOutput::Audio(frame)) => {
                            println!(
                                "  ← Audio: {}ms, {}Hz, {} bytes",
                                frame.duration_ms, frame.sample_rate, frame.data.len()
                            );
                        }
                        Some(PipelineOutput::AgentText(text)) => {
                            println!("  ← Text: \"{text}\"");
                        }
                        Some(PipelineOutput::Metrics(m)) => {
                            println!("  ← Metrics: tts={:.1}ms", m.tts_latency_ms);
                        }
                        _ => break,
                    }
                }
            }
        }
        if i < texts.len() - 1 {
            println!();
        }
    }

    // Read pipeline metrics
    let metrics = handle.metrics.read().await;
    println!("\nPipeline metrics:");
    println!("  TTS latency:  {:.1}ms", metrics.tts_latency_ms);
    println!("  Total audio:  {}ms", metrics.total_audio_ms);
    drop(metrics);

    // Graceful shutdown
    println!("\nSending stop...");
    handle.input_tx.send(PipelineInput::Control(PipelineControl::Stop)).await?;
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    println!("Done!");
    Ok(())
}
