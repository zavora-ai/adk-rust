//! VAD-driven turn-taking manager.
//!
//! Consumes an [`AudioStream`] of [`AudioFrame`] values, applies voice
//! activity detection via [`VadProcessor`], and emits
//! [`VoiceActivityEvent`] values through a registered callback to
//! coordinate agent turn-taking.
//!
//! Two interaction modes are supported:
//!
//! - **HandsFree** — automatic speech boundary detection using configurable
//!   silence and speech duration thresholds.
//! - **PushToTalk** — no automatic events; the caller controls gating
//!   externally.
//!
//! # Example
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_audio::desktop::{VadTurnManager, VadConfig, VadMode, VoiceActivityEvent};
//!
//! let vad: Arc<dyn VadProcessor> = /* ... */;
//! let config = VadConfig {
//!     mode: VadMode::HandsFree,
//!     silence_threshold_ms: 500,
//!     speech_threshold_ms: 200,
//! };
//! let mut manager = VadTurnManager::new(vad, config)?;
//! manager.start(stream, |event| {
//!     match event {
//!         VoiceActivityEvent::SpeechStarted => println!("User started speaking"),
//!         VoiceActivityEvent::SpeechEnded { duration_ms } => {
//!             println!("User stopped speaking after {duration_ms} ms");
//!         }
//!     }
//! });
//! ```

use std::sync::Arc;

use tokio::task::JoinHandle;

use crate::error::{AudioError, AudioResult};
use crate::traits::VadProcessor;

use super::capture::AudioStream;

/// Interaction mode for VAD turn-taking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadMode {
    /// VAD-driven automatic turn boundaries.
    ///
    /// The [`VadTurnManager`] emits [`VoiceActivityEvent::SpeechStarted`]
    /// after consecutive speech exceeds the speech threshold, and
    /// [`VoiceActivityEvent::SpeechEnded`] after consecutive silence
    /// exceeds the silence threshold following speech.
    HandsFree,
    /// Externally gated — speech detection deferred to push-to-talk signal.
    ///
    /// The [`VadTurnManager`] consumes frames but does not emit any
    /// events automatically.
    PushToTalk,
}

/// Configuration for VAD turn-taking.
///
/// Controls the interaction mode and the duration thresholds (in
/// milliseconds) that determine when speech start and end events are
/// emitted.
///
/// # Validation
///
/// Call [`validate()`](VadConfig::validate) before passing to
/// [`VadTurnManager::new()`] — the constructor validates automatically,
/// but explicit validation lets you catch errors earlier.
#[derive(Debug, Clone)]
pub struct VadConfig {
    /// Interaction mode.
    pub mode: VadMode,
    /// Consecutive silence duration (ms) before emitting `SpeechEnded`.
    pub silence_threshold_ms: u32,
    /// Consecutive speech duration (ms) before emitting `SpeechStarted`.
    pub speech_threshold_ms: u32,
}

impl VadConfig {
    /// Validate the configuration.
    ///
    /// Rejects configurations where either threshold is zero.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Vad`] with a descriptive message if either
    /// `silence_threshold_ms` or `speech_threshold_ms` is zero.
    pub fn validate(&self) -> AudioResult<()> {
        if self.silence_threshold_ms == 0 {
            return Err(AudioError::Vad(
                "invalid silence threshold: 0 ms. Threshold must be a positive integer.".into(),
            ));
        }
        if self.speech_threshold_ms == 0 {
            return Err(AudioError::Vad(
                "invalid speech threshold: 0 ms. Threshold must be a positive integer.".into(),
            ));
        }
        Ok(())
    }
}

/// Events emitted by [`VadTurnManager`].
///
/// These events indicate speech boundary transitions detected by the
/// VAD processor in [`VadMode::HandsFree`] mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceActivityEvent {
    /// Speech detected after the speech threshold duration of
    /// consecutive speech frames.
    SpeechStarted,
    /// Silence detected after the silence threshold duration of
    /// consecutive silence frames following speech. Contains the
    /// total speech duration in milliseconds.
    SpeechEnded {
        /// Duration of the preceding speech segment in milliseconds.
        duration_ms: u32,
    },
}

/// VAD-driven turn-taking manager.
///
/// Consumes an [`AudioStream`], applies [`VadProcessor`] to each frame,
/// and emits [`VoiceActivityEvent`] values via a registered callback.
///
/// # Thread Safety
///
/// `VadTurnManager` is `Send + Sync`, making it safe to share across
/// async tasks in a Tokio application.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use adk_audio::desktop::{VadTurnManager, VadConfig, VadMode};
///
/// let mut manager = VadTurnManager::new(vad, config)?;
/// manager.start(stream, |event| {
///     println!("VAD event: {event:?}");
/// });
/// // Later...
/// manager.stop();
/// ```
pub struct VadTurnManager {
    /// The VAD processor used for speech detection.
    vad: Arc<dyn VadProcessor>,
    /// Turn-taking configuration.
    config: VadConfig,
    /// Handle to the background processing task.
    task_handle: Option<JoinHandle<()>>,
}

impl VadTurnManager {
    /// Create a new `VadTurnManager` with the given VAD processor and config.
    ///
    /// Validates the config on construction. If the config is invalid,
    /// returns an error immediately.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Vad`] if the config has zero thresholds.
    pub fn new(vad: Arc<dyn VadProcessor>, config: VadConfig) -> AudioResult<Self> {
        config.validate()?;
        Ok(Self { vad, config, task_handle: None })
    }

    /// Start consuming frames from the [`AudioStream`] and detecting turns.
    ///
    /// Spawns a background Tokio task that reads frames, applies VAD,
    /// and invokes the callback on speech boundary events.
    ///
    /// In [`VadMode::HandsFree`] mode, the task tracks consecutive
    /// speech and silence durations and emits events when thresholds
    /// are crossed. In [`VadMode::PushToTalk`] mode, frames are
    /// consumed but no events are emitted.
    ///
    /// The callback is wrapped in [`Arc`] and invoked via
    /// [`tokio::spawn`] for each event so that slow callbacks do not
    /// block frame processing.
    pub fn start(
        &mut self,
        stream: AudioStream,
        callback: impl Fn(VoiceActivityEvent) + Send + Sync + 'static,
    ) {
        let vad = Arc::clone(&self.vad);
        let config = self.config.clone();
        let callback = Arc::new(callback);

        let handle = tokio::spawn(async move {
            Self::run_loop(stream, vad, config, callback).await;
        });

        self.task_handle = Some(handle);
    }

    /// Register a callback via `on_activity` (convenience alias for [`start()`](Self::start)).
    ///
    /// The callback is invoked on a separate task to avoid blocking
    /// frame processing.
    pub fn on_activity(
        &mut self,
        stream: AudioStream,
        callback: impl Fn(VoiceActivityEvent) + Send + Sync + 'static,
    ) {
        self.start(stream, callback);
    }

    /// Stop the turn manager and release resources.
    ///
    /// Aborts the background task if one is running. No-op if no task
    /// is active.
    pub fn stop(&mut self) {
        if let Some(handle) = self.task_handle.take() {
            handle.abort();
        }
    }

    /// Internal processing loop that reads frames and emits events.
    async fn run_loop(
        mut stream: AudioStream,
        vad: Arc<dyn VadProcessor>,
        config: VadConfig,
        callback: Arc<dyn Fn(VoiceActivityEvent) + Send + Sync>,
    ) {
        match config.mode {
            VadMode::HandsFree => {
                Self::run_hands_free(
                    &mut stream,
                    &vad,
                    config.speech_threshold_ms,
                    config.silence_threshold_ms,
                    &callback,
                )
                .await;
            }
            VadMode::PushToTalk => {
                // Consume frames without emitting events.
                while stream.recv().await.is_some() {
                    // No-op: frames are consumed but no events are emitted.
                }
            }
        }
    }

    /// HandsFree mode processing: track speech/silence durations and emit events.
    async fn run_hands_free(
        stream: &mut AudioStream,
        vad: &Arc<dyn VadProcessor>,
        speech_threshold_ms: u32,
        silence_threshold_ms: u32,
        callback: &Arc<dyn Fn(VoiceActivityEvent) + Send + Sync>,
    ) {
        let mut is_speaking = false;
        let mut consecutive_speech_ms: u32 = 0;
        let mut consecutive_silence_ms: u32 = 0;
        let mut speech_start_ms: u32 = 0;

        while let Some(frame) = stream.recv().await {
            let speech = vad.is_speech(&frame);
            let frame_duration = frame.duration_ms;

            if speech {
                // Reset silence counter on speech.
                consecutive_silence_ms = 0;
                consecutive_speech_ms = consecutive_speech_ms.saturating_add(frame_duration);

                if !is_speaking && consecutive_speech_ms >= speech_threshold_ms {
                    // Transition to speaking state.
                    is_speaking = true;
                    speech_start_ms = 0;

                    let cb = Arc::clone(callback);
                    tokio::spawn(async move {
                        cb(VoiceActivityEvent::SpeechStarted);
                    });
                }

                if is_speaking {
                    speech_start_ms = speech_start_ms.saturating_add(frame_duration);
                }
            } else {
                // Silence detected.
                consecutive_speech_ms = 0;

                if is_speaking {
                    consecutive_silence_ms = consecutive_silence_ms.saturating_add(frame_duration);

                    if consecutive_silence_ms >= silence_threshold_ms {
                        // Transition to silence state.
                        is_speaking = false;
                        let duration_ms = speech_start_ms;

                        let cb = Arc::clone(callback);
                        tokio::spawn(async move {
                            cb(VoiceActivityEvent::SpeechEnded { duration_ms });
                        });

                        // Reset counters.
                        consecutive_silence_ms = 0;
                        speech_start_ms = 0;
                    }
                } else {
                    // Not speaking and silence — just reset counters.
                    consecutive_silence_ms = 0;
                    speech_start_ms = 0;
                }
            }
        }
    }
}

// Static assertions for Send + Sync.
#[allow(dead_code)]
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<VadTurnManager>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::AudioFrame;
    use crate::traits::SpeechSegment;

    /// Minimal mock VadProcessor for unit tests.
    struct AlwaysSpeechVad;

    impl VadProcessor for AlwaysSpeechVad {
        fn is_speech(&self, _frame: &AudioFrame) -> bool {
            true
        }

        fn segment(&self, _frame: &AudioFrame) -> Vec<SpeechSegment> {
            vec![]
        }
    }

    #[test]
    fn test_vad_config_validate_ok() {
        let config = VadConfig {
            mode: VadMode::HandsFree,
            silence_threshold_ms: 500,
            speech_threshold_ms: 200,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_vad_config_validate_zero_silence() {
        let config = VadConfig {
            mode: VadMode::HandsFree,
            silence_threshold_ms: 0,
            speech_threshold_ms: 200,
        };
        let err = config.validate().unwrap_err();
        assert!(matches!(err, AudioError::Vad(msg) if msg.contains("silence threshold")));
    }

    #[test]
    fn test_vad_config_validate_zero_speech() {
        let config = VadConfig {
            mode: VadMode::HandsFree,
            silence_threshold_ms: 500,
            speech_threshold_ms: 0,
        };
        let err = config.validate().unwrap_err();
        assert!(matches!(err, AudioError::Vad(msg) if msg.contains("speech threshold")));
    }

    #[test]
    fn test_vad_config_validate_both_zero() {
        let config = VadConfig {
            mode: VadMode::PushToTalk,
            silence_threshold_ms: 0,
            speech_threshold_ms: 0,
        };
        // Should fail on the first zero field (silence).
        let err = config.validate().unwrap_err();
        assert!(matches!(err, AudioError::Vad(_)));
    }

    #[test]
    fn test_vad_turn_manager_new_valid() {
        let vad: Arc<dyn VadProcessor> = Arc::new(AlwaysSpeechVad);
        let config = VadConfig {
            mode: VadMode::HandsFree,
            silence_threshold_ms: 500,
            speech_threshold_ms: 200,
        };
        let manager = VadTurnManager::new(vad, config);
        assert!(manager.is_ok());
    }

    #[test]
    fn test_vad_turn_manager_new_invalid() {
        let vad: Arc<dyn VadProcessor> = Arc::new(AlwaysSpeechVad);
        let config = VadConfig {
            mode: VadMode::HandsFree,
            silence_threshold_ms: 0,
            speech_threshold_ms: 200,
        };
        let manager = VadTurnManager::new(vad, config);
        assert!(manager.is_err());
    }

    #[test]
    fn test_vad_turn_manager_stop_idempotent() {
        let vad: Arc<dyn VadProcessor> = Arc::new(AlwaysSpeechVad);
        let config = VadConfig {
            mode: VadMode::HandsFree,
            silence_threshold_ms: 500,
            speech_threshold_ms: 200,
        };
        let mut manager = VadTurnManager::new(vad, config).unwrap();
        // Calling stop on a fresh instance should be a no-op.
        manager.stop();
        assert!(manager.task_handle.is_none());
        // Calling again should also be fine.
        manager.stop();
        assert!(manager.task_handle.is_none());
    }

    #[test]
    fn test_vad_turn_manager_is_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<VadTurnManager>();
        assert_sync::<VadTurnManager>();
    }

    #[test]
    fn test_vad_mode_clone_copy() {
        let mode = VadMode::HandsFree;
        let cloned = mode;
        assert_eq!(mode, cloned);

        let mode2 = VadMode::PushToTalk;
        let cloned2 = mode2;
        assert_eq!(mode2, cloned2);
    }

    #[test]
    fn test_voice_activity_event_clone_eq() {
        let event1 = VoiceActivityEvent::SpeechStarted;
        let event2 = event1.clone();
        assert_eq!(event1, event2);

        let event3 = VoiceActivityEvent::SpeechEnded { duration_ms: 1500 };
        let event4 = event3.clone();
        assert_eq!(event3, event4);

        assert_ne!(event1, event3);
    }
}
