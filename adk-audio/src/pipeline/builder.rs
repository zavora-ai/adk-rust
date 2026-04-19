//! Audio pipeline builder for composing processing topologies.

use std::sync::Arc;

use tokio::sync::{RwLock, mpsc, oneshot};

use crate::error::{AudioError, AudioResult};
use crate::pipeline::handle::PipelineHandle;
use crate::pipeline::types::{PipelineInput, PipelineMetrics, PipelineOutput};
use crate::pipeline::voice_agent::{validate_voice_agent_config, voice_agent_loop};
use crate::traits::{
    AudioProcessor, FxChain, MusicProvider, SttProvider, TtsProvider, TtsRequest, VadProcessor,
};

/// Builder for constructing audio pipelines.
///
/// # Example
///
/// ```ignore
/// let handle = AudioPipelineBuilder::new()
///     .tts(my_tts)
///     .stt(my_stt)
///     .vad(my_vad)
///     .agent(my_agent)
///     .build_voice_agent()?;
/// ```
pub struct AudioPipelineBuilder {
    tts: Option<Arc<dyn TtsProvider>>,
    stt: Option<Arc<dyn SttProvider>>,
    music: Option<Arc<dyn MusicProvider>>,
    vad: Option<Arc<dyn VadProcessor>>,
    pre_fx: Option<FxChain>,
    post_fx: Option<FxChain>,
    agent: Option<Arc<dyn adk_core::Agent>>,
    buffer_size: usize,
    /// Desktop audio capture source (microphone).
    #[cfg(feature = "desktop-audio")]
    capture: Option<crate::desktop::capture::AudioCapture>,
    /// Desktop audio playback sink (speaker).
    #[cfg(feature = "desktop-audio")]
    playback: Option<crate::desktop::playback::AudioPlayback>,
}

impl AudioPipelineBuilder {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self {
            tts: None,
            stt: None,
            music: None,
            vad: None,
            pre_fx: None,
            post_fx: None,
            agent: None,
            buffer_size: 32,
            #[cfg(feature = "desktop-audio")]
            capture: None,
            #[cfg(feature = "desktop-audio")]
            playback: None,
        }
    }

    /// Set the TTS provider.
    pub fn tts(mut self, tts: Arc<dyn TtsProvider>) -> Self {
        self.tts = Some(tts);
        self
    }

    /// Set the STT provider.
    pub fn stt(mut self, stt: Arc<dyn SttProvider>) -> Self {
        self.stt = Some(stt);
        self
    }

    /// Set the music generation provider.
    pub fn music(mut self, music: Arc<dyn MusicProvider>) -> Self {
        self.music = Some(music);
        self
    }

    /// Set the VAD processor.
    pub fn vad(mut self, vad: Arc<dyn VadProcessor>) -> Self {
        self.vad = Some(vad);
        self
    }

    /// Set the pre-processing FX chain (applied before STT/VAD).
    pub fn pre_fx(mut self, fx: FxChain) -> Self {
        self.pre_fx = Some(fx);
        self
    }

    /// Set the post-processing FX chain (applied after TTS).
    pub fn post_fx(mut self, fx: FxChain) -> Self {
        self.post_fx = Some(fx);
        self
    }

    /// Set the agent for voice agent pipelines.
    pub fn agent(mut self, agent: Arc<dyn adk_core::Agent>) -> Self {
        self.agent = Some(agent);
        self
    }

    /// Set the channel buffer size (default: 32).
    pub fn buffer_size(mut self, size: usize) -> Self {
        self.buffer_size = size;
        self
    }

    /// Set the audio capture source for desktop pipelines.
    ///
    /// When both `capture` and `playback` are configured, `build_voice_agent()`
    /// will store them for the caller to wire into the pipeline's input/output
    /// channels.
    ///
    /// Only available when the `desktop-audio` feature is enabled.
    #[cfg(feature = "desktop-audio")]
    pub fn capture(mut self, capture: crate::desktop::capture::AudioCapture) -> Self {
        self.capture = Some(capture);
        self
    }

    /// Set the audio playback sink for desktop pipelines.
    ///
    /// When both `capture` and `playback` are configured, `build_voice_agent()`
    /// will store them for the caller to wire into the pipeline's input/output
    /// channels.
    ///
    /// Only available when the `desktop-audio` feature is enabled.
    #[cfg(feature = "desktop-audio")]
    pub fn playback(mut self, playback: crate::desktop::playback::AudioPlayback) -> Self {
        self.playback = Some(playback);
        self
    }

    /// Build a TTS-only pipeline (Text → TTS → Audio).
    pub fn build_tts(self) -> AudioResult<PipelineHandle> {
        let tts = self.tts.ok_or_else(|| {
            AudioError::PipelineClosed("TTS pipeline requires a TtsProvider".into())
        })?;

        let (input_tx, mut input_rx) = mpsc::channel::<PipelineInput>(self.buffer_size);
        let (output_tx, output_rx) = mpsc::channel::<PipelineOutput>(self.buffer_size);
        let metrics = Arc::new(RwLock::new(PipelineMetrics::default()));
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

        let m = metrics.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    input = input_rx.recv() => {
                        let Some(PipelineInput::Text(text)) = input else {
                            if input.is_none() { break; }
                            continue;
                        };
                        let request = TtsRequest { text, ..Default::default() };
                        if let Ok(frame) = tts.synthesize(&request).await {
                            let mut metrics = m.write().await;
                            metrics.total_audio_ms += frame.duration_ms as u64;
                            let _ = output_tx.send(PipelineOutput::Audio(frame)).await;
                        }
                    }
                }
            }
        });

        Ok(PipelineHandle::new(input_tx, output_rx, metrics, shutdown_tx))
    }

    /// Build an STT-only pipeline (Audio → STT → Transcript).
    pub fn build_stt(self) -> AudioResult<PipelineHandle> {
        let stt = self.stt.ok_or_else(|| {
            AudioError::PipelineClosed("STT pipeline requires an SttProvider".into())
        })?;

        let (input_tx, mut input_rx) = mpsc::channel::<PipelineInput>(self.buffer_size);
        let (output_tx, output_rx) = mpsc::channel::<PipelineOutput>(self.buffer_size);
        let metrics = Arc::new(RwLock::new(PipelineMetrics::default()));
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

        let m = metrics.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    input = input_rx.recv() => {
                        let Some(PipelineInput::Audio(frame)) = input else {
                            if input.is_none() { break; }
                            continue;
                        };
                        let opts = crate::traits::SttOptions::default();
                        if let Ok(transcript) = stt.transcribe(&frame, &opts).await {
                            let mut metrics = m.write().await;
                            metrics.total_audio_ms += frame.duration_ms as u64;
                            let _ = output_tx.send(PipelineOutput::Transcript(transcript)).await;
                        }
                    }
                }
            }
        });

        Ok(PipelineHandle::new(input_tx, output_rx, metrics, shutdown_tx))
    }

    /// Build a voice agent pipeline (Audio → VAD → STT → Agent → TTS → Audio).
    ///
    /// Requires `tts`, `stt`, `vad`, and `agent` to be set.
    ///
    /// When the `desktop-audio` feature is enabled and both `capture` and `playback`
    /// are configured, the caller should use the returned [`PipelineHandle`] to wire
    /// the capture stream into `input_tx` and route `output_rx` audio frames to
    /// playback. Starting capture requires a device ID and [`CaptureConfig`](crate::desktop::CaptureConfig),
    /// and playback requires a device ID, so the builder stores the instances and
    /// the caller completes the wiring at runtime.
    pub fn build_voice_agent(self) -> AudioResult<PipelineHandle> {
        validate_voice_agent_config(
            self.tts.is_some(),
            self.stt.is_some(),
            self.vad.is_some(),
            self.agent.is_some(),
        )?;

        let tts = self.tts.unwrap();
        let stt = self.stt.unwrap();
        let vad = self.vad.unwrap();
        let agent = self.agent.unwrap();

        let (input_tx, input_rx) = mpsc::channel::<PipelineInput>(self.buffer_size);
        let (output_tx, output_rx) = mpsc::channel::<PipelineOutput>(self.buffer_size);
        let metrics = Arc::new(RwLock::new(PipelineMetrics::default()));
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let m = metrics.clone();
        tokio::spawn(voice_agent_loop(
            input_rx,
            output_tx,
            stt,
            tts,
            vad,
            agent,
            self.pre_fx,
            self.post_fx,
            m,
            shutdown_rx,
        ));

        // When desktop-audio is enabled and both capture and playback are
        // configured, wire them into the pipeline's input/output channels.
        // Starting capture requires a device_id and CaptureConfig, and
        // playback requires a device_id — these are runtime parameters.
        // The caller should:
        //   1. Call `capture.start_capture(device_id, &config)` to get an AudioStream
        //   2. Spawn a task that reads from the AudioStream and sends
        //      `PipelineInput::Audio(frame)` into `handle.input_tx`
        //   3. Spawn a task that reads `PipelineOutput::Audio(frame)` from
        //      `handle.output_rx` and calls `playback.play(device_id, &frame)`
        #[cfg(feature = "desktop-audio")]
        if self.capture.is_some() && self.playback.is_some() {
            tracing::info!(
                "desktop audio capture and playback configured — caller must wire \
                 capture stream to input_tx and output_rx to playback using device \
                 IDs at runtime"
            );
        }

        Ok(PipelineHandle::new(input_tx, output_rx, metrics, shutdown_tx))
    }

    /// Build a transform-only pipeline (Audio → FxChain → Audio).
    pub fn build_transform(self) -> AudioResult<PipelineHandle> {
        let pre_fx = self.pre_fx.unwrap_or_default();

        let (input_tx, mut input_rx) = mpsc::channel::<PipelineInput>(self.buffer_size);
        let (output_tx, output_rx) = mpsc::channel::<PipelineOutput>(self.buffer_size);
        let metrics = Arc::new(RwLock::new(PipelineMetrics::default()));
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

        let m = metrics.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    input = input_rx.recv() => {
                        let Some(PipelineInput::Audio(frame)) = input else {
                            if input.is_none() { break; }
                            continue;
                        };
                        if let Ok(processed) = pre_fx.process(&frame).await {
                            let mut metrics = m.write().await;
                            metrics.total_audio_ms += processed.duration_ms as u64;
                            let _ = output_tx.send(PipelineOutput::Audio(processed)).await;
                        }
                    }
                }
            }
        });

        Ok(PipelineHandle::new(input_tx, output_rx, metrics, shutdown_tx))
    }

    /// Build a music generation pipeline (Text → MusicProvider → Audio).
    pub fn build_music(self) -> AudioResult<PipelineHandle> {
        let music = self.music.ok_or_else(|| {
            AudioError::PipelineClosed("Music pipeline requires a MusicProvider".into())
        })?;

        let (input_tx, mut input_rx) = mpsc::channel::<PipelineInput>(self.buffer_size);
        let (output_tx, output_rx) = mpsc::channel::<PipelineOutput>(self.buffer_size);
        let metrics = Arc::new(RwLock::new(PipelineMetrics::default()));
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

        let m = metrics.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    input = input_rx.recv() => {
                        let Some(PipelineInput::Text(prompt)) = input else {
                            if input.is_none() { break; }
                            continue;
                        };
                        let request = crate::traits::MusicRequest {
                            prompt,
                            ..Default::default()
                        };
                        if let Ok(frame) = music.generate(&request).await {
                            let mut metrics = m.write().await;
                            metrics.total_audio_ms += frame.duration_ms as u64;
                            let _ = output_tx.send(PipelineOutput::Audio(frame)).await;
                        }
                    }
                }
            }
        });

        Ok(PipelineHandle::new(input_tx, output_rx, metrics, shutdown_tx))
    }
}

impl Default for AudioPipelineBuilder {
    fn default() -> Self {
        Self::new()
    }
}
