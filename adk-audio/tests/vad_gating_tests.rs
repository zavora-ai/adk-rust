//! Property P7: VAD Gating
//!
//! *For any* sequence of AudioFrame values where VadProcessor::is_speech returns
//! false for all frames, the voice agent pipeline SHALL NOT invoke the SttProvider.
//!
//! **Validates: Requirements 6, 8**

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use adk_audio::{
    AudioFrame, AudioPipelineBuilder, AudioResult, SpeechSegment, SttOptions, SttProvider,
    Transcript, TtsProvider, TtsRequest, VadProcessor, Voice,
};
use async_trait::async_trait;
use futures::Stream;
use proptest::prelude::*;

// --- Mock that always says "no speech" ---
struct AlwaysSilentVad;
impl VadProcessor for AlwaysSilentVad {
    fn is_speech(&self, _frame: &AudioFrame) -> bool {
        false
    }
    fn segment(&self, _frame: &AudioFrame) -> Vec<SpeechSegment> {
        vec![]
    }
}

// --- Mock that always says "speech" (used in future tests) ---
#[allow(dead_code)]
struct AlwaysSpeechVad;
impl VadProcessor for AlwaysSpeechVad {
    fn is_speech(&self, _frame: &AudioFrame) -> bool {
        true
    }
    fn segment(&self, _frame: &AudioFrame) -> Vec<SpeechSegment> {
        vec![SpeechSegment { start_ms: 0, end_ms: 100 }]
    }
}

// --- Counting STT mock ---
struct CountingStt {
    call_count: Arc<AtomicU32>,
}

#[async_trait]
impl SttProvider for CountingStt {
    async fn transcribe(&self, _audio: &AudioFrame, _opts: &SttOptions) -> AudioResult<Transcript> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        Ok(Transcript { text: "hello".into(), ..Default::default() })
    }
    async fn transcribe_stream(
        &self,
        _audio: Pin<Box<dyn Stream<Item = AudioFrame> + Send>>,
        _opts: &SttOptions,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<Transcript>> + Send>>> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

struct StubTts;
#[async_trait]
impl TtsProvider for StubTts {
    async fn synthesize(&self, _: &TtsRequest) -> AudioResult<AudioFrame> {
        Ok(AudioFrame::silence(16000, 1, 100))
    }
    async fn synthesize_stream(
        &self,
        _: &TtsRequest,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<AudioFrame>> + Send>>> {
        Ok(Box::pin(futures::stream::empty()))
    }
    fn voice_catalog(&self) -> &[Voice] {
        &[]
    }
}

struct StubAgent;
#[async_trait]
impl adk_core::Agent for StubAgent {
    fn name(&self) -> &str {
        "stub"
    }
    fn description(&self) -> &str {
        "stub"
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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]

    /// P7.1: All-silence frames never trigger STT
    #[test]
    fn prop_silence_no_stt(n_frames in 1usize..20) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let stt_count = Arc::new(AtomicU32::new(0));
            let stt = Arc::new(CountingStt { call_count: stt_count.clone() });

            let mut handle = AudioPipelineBuilder::new()
                .tts(Arc::new(StubTts))
                .stt(stt)
                .vad(Arc::new(AlwaysSilentVad))
                .agent(Arc::new(StubAgent))
                .build_voice_agent()
                .unwrap();

            // Send silence frames
            for _ in 0..n_frames {
                let frame = AudioFrame::silence(16000, 1, 100);
                let _ = handle.input_tx.send(adk_audio::PipelineInput::Audio(frame)).await;
            }

            // Give pipeline time to process
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            handle.shutdown();

            let count = stt_count.load(Ordering::SeqCst);
            prop_assert_eq!(count, 0, "STT should not be called for all-silence frames");
            Ok(())
        })?;
    }
}
