//! Property-based tests for error message context preservation.
//!
//! **Feature: realtime-audio-transport, Property 5: Error Message Context Preservation**
//! *For any* non-empty context string, constructing a `RealtimeError::OpusCodecError`,
//! `RealtimeError::WebRTCError`, or `RealtimeError::LiveKitError` with that context string
//! SHALL produce a `Display` output that contains the original context string.
//! **Validates: Requirements 14.4**

use adk_realtime::RealtimeError;
use proptest::prelude::*;

/// Generator for non-empty context strings.
fn arb_non_empty_string() -> impl Strategy<Value = String> {
    ".{1,200}".prop_filter("must be non-empty", |s| !s.is_empty())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: realtime-audio-transport, Property 5: Error Message Context Preservation**
    /// *For any* non-empty context string, constructing `RealtimeError::OpusCodecError`
    /// via the convenience constructor SHALL produce a Display output containing the original string.
    /// **Validates: Requirements 14.4**
    #[test]
    fn prop_opus_codec_error_preserves_context(ctx in arb_non_empty_string()) {
        let error = RealtimeError::opus(&ctx);
        let display = format!("{}", error);
        prop_assert!(
            display.contains(&ctx),
            "OpusCodecError display '{}' does not contain context '{}'",
            display,
            ctx
        );
    }

    /// **Feature: realtime-audio-transport, Property 5: Error Message Context Preservation**
    /// *For any* non-empty context string, constructing `RealtimeError::WebRTCError`
    /// via the convenience constructor SHALL produce a Display output containing the original string.
    /// **Validates: Requirements 14.4**
    #[test]
    fn prop_webrtc_error_preserves_context(ctx in arb_non_empty_string()) {
        let error = RealtimeError::webrtc(&ctx);
        let display = format!("{}", error);
        prop_assert!(
            display.contains(&ctx),
            "WebRTCError display '{}' does not contain context '{}'",
            display,
            ctx
        );
    }

    /// **Feature: realtime-audio-transport, Property 5: Error Message Context Preservation**
    /// *For any* non-empty context string, constructing `RealtimeError::LiveKitError`
    /// via the convenience constructor SHALL produce a Display output containing the original string.
    /// **Validates: Requirements 14.4**
    #[test]
    fn prop_livekit_error_preserves_context(ctx in arb_non_empty_string()) {
        let error = RealtimeError::livekit(&ctx);
        let display = format!("{}", error);
        prop_assert!(
            display.contains(&ctx),
            "LiveKitError display '{}' does not contain context '{}'",
            display,
            ctx
        );
    }

    /// **Feature: realtime-audio-transport, Property 5: Error Message Context Preservation**
    /// *For any* non-empty context string, constructing error variants directly (not via
    /// convenience constructors) SHALL also preserve the context string in Display output.
    /// **Validates: Requirements 14.4**
    #[test]
    fn prop_direct_variant_construction_preserves_context(ctx in arb_non_empty_string()) {
        let opus = RealtimeError::OpusCodecError(ctx.clone());
        let webrtc = RealtimeError::WebRTCError(ctx.clone());
        let livekit = RealtimeError::LiveKitError(ctx.clone());

        let opus_display = format!("{}", opus);
        let webrtc_display = format!("{}", webrtc);
        let livekit_display = format!("{}", livekit);

        prop_assert!(
            opus_display.contains(&ctx),
            "Direct OpusCodecError display '{}' does not contain context '{}'",
            opus_display,
            ctx
        );
        prop_assert!(
            webrtc_display.contains(&ctx),
            "Direct WebRTCError display '{}' does not contain context '{}'",
            webrtc_display,
            ctx
        );
        prop_assert!(
            livekit_display.contains(&ctx),
            "Direct LiveKitError display '{}' does not contain context '{}'",
            livekit_display,
            ctx
        );
    }
}
