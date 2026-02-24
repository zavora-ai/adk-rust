//! Property P3: Pipeline Builder Validation
//!
//! *For any* call to `build_voice_agent()`, if any of `tts`, `stt`, `vad`,
//! or `agent` is not set, the builder SHALL return an error.
//! If all four are set, the builder SHALL return a valid `PipelineHandle`.
//!
//! **Validates: Requirement 7**

use std::pin::Pin;
use std::sync::Arc;

use adk_audio::{
    AudioFrame, AudioPipelineBuilder, AudioResult, SpeechSegment, SttOptions, SttProvider,
    Transcript, TtsProvider, TtsRequest, VadProcessor, Voice,
};
use async_trait::async_trait;
use futures::Stream;
use proptest::prelude::*;

// --- Mock implementations ---

struct MockTts;
#[async_trait]
impl TtsProvider for MockTts {
    async fn synthesize(&self, _req: &TtsRequest) -> AudioResult<AudioFrame> {
        Ok(AudioFrame::silence(16000, 1, 100))
    }
    async fn synthesize_stream(
        &self,
        _req: &TtsRequest,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<AudioFrame>> + Send>>> {
        Ok(Box::pin(futures::stream::empty()))
    }
    fn voice_catalog(&self) -> &[Voice] {
        &[]
    }
}

struct MockStt;
#[async_trait]
impl SttProvider for MockStt {
    async fn transcribe(&self, _audio: &AudioFrame, _opts: &SttOptions) -> AudioResult<Transcript> {
        Ok(Transcript::default())
    }
    async fn transcribe_stream(
        &self,
        _audio: Pin<Box<dyn Stream<Item = AudioFrame> + Send>>,
        _opts: &SttOptions,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<Transcript>> + Send>>> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

struct MockVad;
impl VadProcessor for MockVad {
    fn is_speech(&self, _frame: &AudioFrame) -> bool {
        false
    }
    fn segment(&self, _frame: &AudioFrame) -> Vec<SpeechSegment> {
        vec![]
    }
}

struct MockAgent;
#[async_trait]
impl adk_core::Agent for MockAgent {
    fn name(&self) -> &str {
        "mock"
    }
    fn description(&self) -> &str {
        "mock agent"
    }
    fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
        &[]
    }
    async fn run(
        &self,
        _ctx: Arc<dyn adk_core::InvocationContext>,
    ) -> adk_core::Result<adk_core::EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

// --- Property tests ---

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// P3.1: Missing components → error
    #[test]
    fn prop_missing_components_error(
        has_tts in any::<bool>(),
        has_stt in any::<bool>(),
        has_vad in any::<bool>(),
        has_agent in any::<bool>(),
    ) {
        if has_tts && has_stt && has_vad && has_agent {
            // All present — skip (tested in P3.2)
            return Ok(());
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let mut builder = AudioPipelineBuilder::new();
            if has_tts {
                builder = builder.tts(Arc::new(MockTts));
            }
            if has_stt {
                builder = builder.stt(Arc::new(MockStt));
            }
            if has_vad {
                builder = builder.vad(Arc::new(MockVad));
            }
            if has_agent {
                builder = builder.agent(Arc::new(MockAgent));
            }
            let result = builder.build_voice_agent();
            prop_assert!(result.is_err(), "expected error when components missing");
            Ok(())
        })?;
    }

    /// P3.2: All components set → success
    #[test]
    fn prop_all_components_success(_dummy in 0..1u8) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let builder = AudioPipelineBuilder::new()
                .tts(Arc::new(MockTts))
                .stt(Arc::new(MockStt))
                .vad(Arc::new(MockVad))
                .agent(Arc::new(MockAgent));
            let result = builder.build_voice_agent();
            prop_assert!(result.is_ok(), "expected success with all components");
            Ok(())
        })?;
    }
}
