//! Property P9: Error Context Preservation
//!
//! *For any* `AudioError` variant, the `Display` implementation SHALL include
//! the subsystem name and actionable context.
//!
//! **Validates: Requirement 11**

use adk_audio::AudioError;
use proptest::prelude::*;

fn arb_context() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 _-]{1,50}"
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// P9.1: TTS errors include provider name
    #[test]
    fn prop_tts_error_context(provider in arb_context(), message in arb_context()) {
        let err = AudioError::Tts { provider: provider.clone(), message: message.clone() };
        let display = format!("{err}");
        prop_assert!(display.contains("TTS"), "missing TTS prefix: {display}");
        prop_assert!(display.contains(&provider), "missing provider: {display}");
        prop_assert!(display.contains(&message), "missing message: {display}");
    }

    /// P9.2: STT errors include provider name
    #[test]
    fn prop_stt_error_context(provider in arb_context(), message in arb_context()) {
        let err = AudioError::Stt { provider: provider.clone(), message: message.clone() };
        let display = format!("{err}");
        prop_assert!(display.contains("STT"), "missing STT prefix: {display}");
        prop_assert!(display.contains(&provider), "missing provider: {display}");
    }

    /// P9.3: Codec errors include context
    #[test]
    fn prop_codec_error_context(message in arb_context()) {
        let err = AudioError::Codec(message.clone());
        let display = format!("{err}");
        prop_assert!(display.contains("Codec"), "missing Codec prefix: {display}");
        prop_assert!(display.contains(&message), "missing message: {display}");
    }

    /// P9.4: Pipeline errors include context
    #[test]
    fn prop_pipeline_error_context(message in arb_context()) {
        let err = AudioError::PipelineClosed(message.clone());
        let display = format!("{err}");
        prop_assert!(display.contains("Pipeline"), "missing Pipeline prefix: {display}");
        prop_assert!(display.contains(&message), "missing message: {display}");
    }
}
