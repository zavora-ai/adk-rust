//! `adk-rust --voice`: the console side of the harness voice surface (spec 3.1 §8.1,
//! §7.17, H5).
//!
//! The surface itself is `adk_harness::VoiceSurface`; this module picks the realtime
//! provider, moves audio between the surface and the console, and prints the transcript,
//! the held calls and their decisions. Without `voice-desktop`, audio comes from
//! `--voice-input` (a WAV answering the first held call) and goes to `--voice-output`.

use std::path::Path;
use std::sync::Arc;

use adk_harness::{VoiceEvent, VoiceSurface};
use adk_model::ModelProvider;
use adk_realtime::audio::AudioChunk;
use adk_realtime::model::BoxedModel;
use adk_realtime::{RealtimeModel, ServerEvent};
use anyhow::{Context, Result, anyhow, bail};

use crate::ResolvedModel;
use crate::cli::Provider;
use crate::harness_cli::EXIT_HELD;

/// The sample rate of the model's audio, for `--voice-output` and playback.
const OUTPUT_SAMPLE_RATE: u32 = 24_000;

/// How long a run without a microphone waits for the provider's next event before it ends.
const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// The realtime model `--voice` runs on, its display id, and the sample rate of the mono
/// PCM16 audio it takes: Gemini Live declares 16 kHz on every chunk, the OpenAI Realtime
/// API reads PCM16 as 24 kHz; a scripted provider takes either.
pub(crate) struct RealtimeChoice {
    pub(crate) model: BoxedModel,
    pub(crate) id: String,
    pub(crate) input_rate: Option<u32>,
}

/// Picks the realtime provider from the resolved chat model's provider and key, or plays
/// `script` (a `ScriptedRealtimeModel` script, inline JSON or a file) in tests.
pub(crate) fn realtime_model(
    model: &ResolvedModel,
    script: Option<&str>,
) -> Result<RealtimeChoice> {
    #[cfg(feature = "scripted-model")]
    if let Some(script) = script {
        let text = if script.trim_start().starts_with('[') {
            script.to_string()
        } else {
            std::fs::read_to_string(script)
                .with_context(|| format!("reading --voice-script {script}"))?
        };
        let steps: Vec<adk_realtime::scripted::ScriptedStep> = serde_json::from_str(&text)
            .context("--voice-script must be a JSON array of scripted steps")?;
        let scripted = adk_realtime::scripted::ScriptedRealtimeModel::new(steps);
        return Ok(RealtimeChoice {
            id: scripted.model_id().to_string(),
            model: Arc::new(scripted),
            input_rate: None,
        });
    }
    #[cfg(not(feature = "scripted-model"))]
    let _ = script;

    let key = model.api_key.clone().ok_or_else(|| {
        anyhow!("--voice needs the provider's API key; run `adk-rust` once to set it up")
    })?;
    match model.provider {
        Some(Provider::Model(ModelProvider::Gemini)) => {
            let live = adk_realtime::gemini::GeminiRealtimeModel::new(
                adk_realtime::gemini::GeminiLiveBackend::studio(key),
                adk_model::catalog::GEMINI_LIVE_DEFAULT,
            );
            Ok(RealtimeChoice {
                id: live.model_id().to_string(),
                model: Arc::new(live),
                input_rate: Some(16_000),
            })
        }
        Some(Provider::Model(ModelProvider::Openai)) => {
            let live = adk_realtime::openai::OpenAIRealtimeModel::with_default_model(key);
            Ok(RealtimeChoice {
                id: live.model_id().to_string(),
                model: Arc::new(live),
                input_rate: Some(24_000),
            })
        }
        _ => bail!(
            "--voice runs on a realtime provider: pass --provider gemini (Gemini Live) or \
             --provider openai (the OpenAI Realtime API); {} has no realtime API",
            model.id
        ),
    }
}

/// Drives the surface until the session ends: prints what is said and held, answers the
/// first held call from `input` when given, collects the model's audio into `output`.
/// Audio goes to the provider at `input_rate` (16 kHz when the provider takes either).
///
/// Returns the exit code: [`EXIT_HELD`] while a call is still held, 0 otherwise.
pub(crate) async fn run(
    surface: Arc<VoiceSurface>,
    first_turn: Option<&str>,
    input: Option<&Path>,
    output: Option<&Path>,
    input_rate: Option<u32>,
) -> Result<i32> {
    surface.connect().await?;
    if let Some(text) = first_turn {
        println!("[you] {text}");
        surface.say(text).await?;
    }

    #[cfg(feature = "voice-desktop")]
    let desktop = if input.is_none() {
        match desktop::Desktop::start(surface.clone(), input_rate.unwrap_or(16_000)) {
            Ok(desktop) => Some(desktop),
            Err(error) => {
                println!("no desktop audio ({error}); listening to --voice-input only");
                None
            }
        }
    } else {
        None
    };
    #[cfg(feature = "voice-desktop")]
    let interactive = desktop.is_some();
    #[cfg(not(feature = "voice-desktop"))]
    let desktop = ();
    #[cfg(not(feature = "voice-desktop"))]
    let interactive = false;

    let mut answered = false;
    let mut resumed = false;
    let mut saw_call = false;
    let mut spoken = String::new();
    let mut audio_out: Vec<u8> = Vec::new();
    loop {
        let next = if interactive {
            surface.next_event().await
        } else {
            match tokio::time::timeout(IDLE_TIMEOUT, surface.next_event()).await {
                Ok(next) => next,
                Err(_) => {
                    println!("no event for {}s; ending the session", IDLE_TIMEOUT.as_secs());
                    None
                }
            }
        };
        let Some(event) = next else {
            break;
        };
        match event? {
            VoiceEvent::Held { request, .. } => {
                println!(
                    "approval needed: {} {}",
                    request.tool_name,
                    adk_harness::escape_control(&first_line(&request.args.to_string()))
                );
                println!("say yes or no (or decide with `adk-rust approvals`)");
            }
            VoiceEvent::Decided { decision, by, .. } => {
                let decision = match decision {
                    adk_harness::SpokenDecision::Approve => "approve",
                    adk_harness::SpokenDecision::Deny => "deny",
                };
                let by = match by {
                    adk_harness::DecisionSource::Voice => "voice",
                    adk_harness::DecisionSource::Inbox => "inbox",
                };
                println!("decided ({by}): {decision}");
            }
            VoiceEvent::Resumed(call) => {
                resumed = true;
                let outcome = if call.result.get("error").is_some() { "not run" } else { "ran" };
                println!(
                    "{outcome}: {} -> {}",
                    call.name,
                    adk_harness::escape_control(&first_line(&call.result.to_string()))
                );
            }
            VoiceEvent::Provider(event) => match event {
                ServerEvent::InputTranscriptCompleted { transcript, .. } => {
                    println!("[you] {}", adk_harness::escape_control(&transcript));
                }
                ServerEvent::TranscriptDelta { delta, .. }
                | ServerEvent::TextDelta { delta, .. } => {
                    spoken.push_str(&delta);
                }
                ServerEvent::AudioDelta { delta, .. } => {
                    play(&desktop, &delta).await;
                    // Kept for --voice-output only; a played session drops it.
                    if output.is_some() {
                        audio_out.extend_from_slice(&delta);
                    }
                }
                ServerEvent::FunctionCallDone { .. } => saw_call = true,
                ServerEvent::ResponseDone { .. } => {
                    let said = !spoken.trim().is_empty();
                    if said {
                        println!("[harness] {}", adk_harness::escape_control(spoken.trim()));
                    }
                    spoken.clear();
                    let called = std::mem::take(&mut saw_call);
                    // The recording answers once the model has asked about the held call:
                    // its first spoken response after the hold.
                    if let Some(path) = input
                        && !answered
                        && said
                        && !surface.held_calls().is_empty()
                    {
                        send_wav(&surface, path, input_rate).await?;
                        answered = true;
                        continue;
                    }
                    if interactive || called {
                        // The model speaks again once the call it made is answered.
                        continue;
                    }
                    // One exchange per run without a microphone: the spoken answer, or the
                    // resumed call's response. A held call the recording answered waits for
                    // that response; one nothing can answer ends the run as held.
                    let held = !surface.held_calls().is_empty();
                    if resumed || !held || !answered {
                        break;
                    }
                }
                ServerEvent::Error { error, .. } => {
                    println!("provider error: {}", adk_harness::escape_control(&error.message));
                }
                _ => {}
            },
            _ => {}
        }
    }
    if !spoken.trim().is_empty() {
        println!("[harness] {}", adk_harness::escape_control(spoken.trim()));
    }
    if let (Some(path), false) = (output, audio_out.is_empty()) {
        std::fs::write(path, wav_bytes(&audio_out, OUTPUT_SAMPLE_RATE))
            .with_context(|| format!("writing --voice-output {}", path.display()))?;
        println!(
            "audio: {} ({} bytes, {OUTPUT_SAMPLE_RATE} Hz mono)",
            path.display(),
            audio_out.len()
        );
    }
    surface.close().await;
    let held = surface.held_calls();
    if held.is_empty() {
        return Ok(0);
    }
    println!("\n{} call(s) are still held for a decision.", held.len());
    for (_, request) in &held {
        println!("  {} {}", request.tool_name, first_line(&request.args.to_string()));
    }
    println!("Decide with `adk-rust approvals list` and `adk-rust approvals approve ID`.");
    Ok(EXIT_HELD)
}

/// Plays one chunk of the model's audio on the speaker.
#[cfg(feature = "voice-desktop")]
async fn play(desktop: &Option<desktop::Desktop>, pcm: &[u8]) {
    if let Some(desktop) = desktop {
        desktop.play(pcm).await;
    }
}

/// Without `voice-desktop` there is no speaker; the audio goes to `--voice-output` only.
#[cfg(not(feature = "voice-desktop"))]
async fn play(_desktop: &(), _pcm: &[u8]) {}

/// Sends the recording at `path` as the user's turn: a mono PCM16 WAV at the provider's
/// `input_rate`, or 16 kHz or 24 kHz when the provider takes either.
async fn send_wav(surface: &VoiceSurface, path: &Path, input_rate: Option<u32>) -> Result<()> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading --voice-input {}", path.display()))?;
    let frame = adk_audio::decode(&bytes, adk_audio::AudioFormat::Wav)
        .map_err(|error| anyhow!("--voice-input {}: {error}", path.display()))?;
    if frame.channels != 1 {
        bail!(
            "--voice-input {} has {} channels; the providers take mono (sox in.wav -c 1 -r 16000 -b 16 out.wav)",
            path.display(),
            frame.channels
        );
    }
    let format = match (frame.sample_rate, input_rate) {
        (16_000, None | Some(16_000)) => adk_realtime::audio::AudioFormat::pcm16_16khz(),
        (24_000, None | Some(24_000)) => adk_realtime::audio::AudioFormat::pcm16_24khz(),
        (rate, expected) => {
            let expected = expected.unwrap_or(16_000);
            bail!(
                "--voice-input {} is {rate} Hz; this provider takes {expected} Hz mono PCM16 \
                 (sox in.wav -c 1 -r {expected} -b 16 out.wav)",
                path.display()
            )
        }
    };
    println!("[you] (audio: {}, {} ms)", path.display(), frame.duration_ms);
    // Sent as a microphone sends it: 100 ms chunks at the rate they are spoken, then 1.5 s
    // of silence, which is what ends the turn for a provider whose voice activity detection
    // runs server-side.
    let chunk_bytes = (frame.sample_rate as usize / 10) * 2;
    let silence = vec![0u8; frame.sample_rate as usize * 3];
    for chunk in frame.data.chunks(chunk_bytes).chain(silence.chunks(chunk_bytes)) {
        surface.send_audio(&AudioChunk::new(chunk.to_vec(), format.clone())).await?;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    surface.commit_audio().await?;
    Ok(())
}

/// The first line of `text`, trimmed and bounded.
fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > 160 {
        format!("{}…", line.chars().take(160).collect::<String>())
    } else {
        line.to_string()
    }
}

/// A RIFF WAV container around mono PCM16 at `sample_rate`.
pub(crate) fn wav_bytes(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

/// The microphone and the speaker (feature `voice-desktop`).
#[cfg(feature = "voice-desktop")]
mod desktop {
    use std::sync::Arc;

    use adk_audio::{AudioCapture, AudioFrame, AudioPlayback, CaptureConfig};
    use adk_harness::VoiceSurface;
    use adk_realtime::audio::AudioChunk;
    use anyhow::{Result, anyhow};
    use tokio::sync::Mutex;

    pub(super) struct Desktop {
        _capture: AudioCapture,
        playback: Mutex<AudioPlayback>,
        output_device: String,
    }

    impl Desktop {
        /// Opens the default input and output devices and forwards the microphone to
        /// `surface` on its own task.
        pub(super) fn start(surface: Arc<VoiceSurface>, rate: u32) -> Result<Self> {
            let input = AudioCapture::list_input_devices()?
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("no audio input device"))?;
            let output = AudioPlayback::list_output_devices()?
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("no audio output device"))?;
            let mut capture = AudioCapture::new();
            let config = CaptureConfig { sample_rate: rate, ..CaptureConfig::default() };
            let mut stream = capture.start_capture(input.id(), &config)?;
            println!("listening on {} · speaking on {}", input.name(), output.name());
            tokio::spawn(async move {
                while let Some(frame) = stream.recv().await {
                    let data = frame.data.to_vec();
                    let chunk = if rate == 24_000 {
                        AudioChunk::pcm16_24khz(data)
                    } else {
                        AudioChunk::pcm16_16khz(data)
                    };
                    if surface.send_audio(&chunk).await.is_err() {
                        break;
                    }
                }
            });
            Ok(Self {
                _capture: capture,
                playback: Mutex::new(AudioPlayback::new()),
                output_device: output.id().to_string(),
            })
        }

        /// Plays one chunk of the model's audio.
        pub(super) async fn play(&self, pcm: &[u8]) {
            let frame = AudioFrame::new(pcm.to_vec(), super::OUTPUT_SAMPLE_RATE, 1);
            if let Err(error) = self.playback.lock().await.play(&self.output_device, &frame).await {
                tracing::debug!(error = %error, "playback failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_bytes_round_trip_through_the_codec() {
        let pcm: Vec<u8> = (0..320u16).flat_map(|n| n.to_le_bytes()).collect();
        let wav = wav_bytes(&pcm, 16_000);
        let frame = adk_audio::decode(&wav, adk_audio::AudioFormat::Wav).unwrap();
        assert_eq!(frame.sample_rate, 16_000);
        assert_eq!(frame.channels, 1);
        assert_eq!(frame.data.as_ref(), pcm.as_slice());
        assert_eq!(frame.duration_ms, 20);
    }

    #[test]
    fn first_line_is_trimmed_and_bounded() {
        assert_eq!(first_line("  ls -la \nrest"), "ls -la");
        let long = "x".repeat(200);
        assert_eq!(first_line(&long).chars().count(), 161);
    }
}
