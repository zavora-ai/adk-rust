//! Voice agent pipeline loop: Audio → VAD → STT → Agent → SentenceChunker → TTS → Audio.

use std::sync::Arc;

use tokio::sync::{RwLock, mpsc, oneshot};

use crate::error::AudioResult;
use crate::frame::{AudioFrame, merge_frames};
use crate::pipeline::chunker::SentenceChunker;
use crate::pipeline::types::{PipelineControl, PipelineInput, PipelineMetrics, PipelineOutput};
use crate::traits::{FxChain, SttOptions, SttProvider, TtsProvider, TtsRequest, VadProcessor};

/// Run the voice agent pipeline loop.
///
/// Processes audio input through VAD gating, STT transcription, agent
/// reasoning, sentence-chunked TTS synthesis, and audio output.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn voice_agent_loop(
    mut input_rx: mpsc::Receiver<PipelineInput>,
    output_tx: mpsc::Sender<PipelineOutput>,
    stt: Arc<dyn SttProvider>,
    tts: Arc<dyn TtsProvider>,
    vad: Arc<dyn VadProcessor>,
    _agent: Arc<dyn adk_core::Agent>,
    pre_fx: Option<FxChain>,
    _post_fx: Option<FxChain>,
    metrics: Arc<RwLock<PipelineMetrics>>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let mut speech_buffer: Vec<AudioFrame> = Vec::new();
    let mut silence_count = 0u32;
    let silence_threshold = 5; // consecutive silent frames before flush
    let mut total_frames = 0u64;
    let mut speech_frames = 0u64;

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,
            input = input_rx.recv() => {
                let Some(input) = input else { break };
                match input {
                    PipelineInput::Audio(frame) => {
                        total_frames += 1;
                        // 1. Apply pre-FX
                        let frame = if let Some(ref fx) = pre_fx {
                            use crate::traits::AudioProcessor;
                            fx.process(&frame).await.unwrap_or(frame)
                        } else {
                            frame
                        };

                        // 2. VAD gating
                        if vad.is_speech(&frame) {
                            speech_frames += 1;
                            speech_buffer.push(frame);
                            silence_count = 0;
                        } else {
                            silence_count += 1;
                            if silence_count >= silence_threshold && !speech_buffer.is_empty() {
                                // 3. Flush speech buffer → STT
                                let merged = merge_frames(&speech_buffer);
                                speech_buffer.clear();

                                let stt_start = std::time::Instant::now();
                                let transcript = stt.transcribe(&merged, &SttOptions::default()).await;
                                let stt_elapsed = stt_start.elapsed().as_millis() as f64;

                                if let Ok(transcript) = transcript {
                                    {
                                        let mut m = metrics.write().await;
                                        m.stt_latency_ms = stt_elapsed;
                                        if total_frames > 0 {
                                            m.vad_speech_ratio = speech_frames as f32 / total_frames as f32;
                                        }
                                    }
                                    let _ = output_tx.send(PipelineOutput::Transcript(transcript.clone())).await;

                                    // 4. Sentence-chunked TTS (simplified — full agent integration in later task)
                                    process_text_to_speech(&tts, &output_tx, &metrics, &transcript.text).await;
                                }
                            }
                        }
                    }
                    PipelineInput::Text(text) => {
                        process_text_to_speech(&tts, &output_tx, &metrics, &text).await;
                    }
                    PipelineInput::Control(PipelineControl::Stop) => break,
                    PipelineInput::Control(_) => {}
                }
            }
        }
    }
}

/// Synthesize text through sentence-chunked TTS and emit audio output.
async fn process_text_to_speech(
    tts: &Arc<dyn TtsProvider>,
    output_tx: &mpsc::Sender<PipelineOutput>,
    metrics: &Arc<RwLock<PipelineMetrics>>,
    text: &str,
) {
    let mut chunker = SentenceChunker::new();
    let sentences = chunker.push(text);
    let remaining = chunker.flush();

    let all_sentences = sentences.into_iter().chain(remaining).collect::<Vec<_>>();

    for sentence in all_sentences {
        let _ = output_tx.send(PipelineOutput::AgentText(sentence.clone())).await;

        let tts_start = std::time::Instant::now();
        let request = TtsRequest { text: sentence, ..Default::default() };
        if let Ok(frame) = tts.synthesize(&request).await {
            let tts_elapsed = tts_start.elapsed().as_millis() as f64;
            {
                let mut m = metrics.write().await;
                m.tts_latency_ms = tts_elapsed;
                m.total_audio_ms += frame.duration_ms as u64;
            }
            let _ = output_tx.send(PipelineOutput::Audio(frame)).await;
        }
    }
}

/// Helper to create an `AudioResult<PipelineHandle>` for a voice agent pipeline.
pub(crate) fn validate_voice_agent_config(
    has_tts: bool,
    has_stt: bool,
    has_vad: bool,
    has_agent: bool,
) -> AudioResult<()> {
    let mut missing = Vec::new();
    if !has_tts {
        missing.push("tts");
    }
    if !has_stt {
        missing.push("stt");
    }
    if !has_vad {
        missing.push("vad");
    }
    if !has_agent {
        missing.push("agent");
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(crate::error::AudioError::PipelineClosed(format!(
            "voice agent pipeline requires: {}",
            missing.join(", ")
        )))
    }
}
