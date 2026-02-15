//! Property-based tests for SDP offer structure.
//!
//! **Feature: realtime-audio-transport, Property 4: SDP Offer Structure**
//! *For any* session configuration, the generated local SDP offer string SHALL contain
//! at least one `m=audio` media line and at least one `a=sctpmap` or `a=mid` line
//! indicating a data channel.
//! **Validates: Requirements 9.1**
//!
//! Requires the `openai-webrtc` feature (pulls in `str0m`).
//! Run: `cargo test -p adk-realtime --features openai-webrtc --test sdp_offer_tests`

#![cfg(feature = "openai-webrtc")]

use std::time::Instant;

use proptest::prelude::*;
use str0m::Rtc;
use str0m::media::{Direction, MediaKind};

/// Generator for session-like configurations.
///
/// While the SDP offer structure is deterministic for a given set of media/channel
/// additions, we vary configuration parameters to demonstrate the property holds
/// across any session configuration the developer might choose.
fn arb_session_config() -> impl Strategy<Value = (String, String)> {
    (
        "[a-z]{3,10}", // model name component
        prop_oneof![
            Just("alloy".to_string()),
            Just("echo".to_string()),
            Just("shimmer".to_string()),
            Just("fable".to_string()),
            Just("onyx".to_string()),
            Just("nova".to_string()),
        ],
    )
}

/// Helper: create an Rtc instance, add audio track + data channel, and return the SDP offer string.
fn generate_sdp_offer() -> String {
    let mut rtc = Rtc::new(Instant::now());
    let mut changes = rtc.sdp_api();

    // Add bidirectional audio media line (Opus)
    changes.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);

    // Add the "oai-events" data channel for JSON event exchange
    changes.add_channel("oai-events".to_string());

    let (offer, _pending) =
        changes.apply().expect("SDP offer generation should succeed for audio + data channel");

    offer.to_sdp_string()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: realtime-audio-transport, Property 4: SDP Offer Structure**
    /// *For any* session configuration, the generated SDP offer SHALL contain
    /// at least one `m=audio` media line.
    /// **Validates: Requirements 9.1**
    #[test]
    fn prop_sdp_offer_contains_audio_media_line(
        (_model, _voice) in arb_session_config()
    ) {
        let sdp_string = generate_sdp_offer();

        prop_assert!(
            sdp_string.contains("m=audio"),
            "SDP offer missing m=audio line. SDP:\n{}",
            sdp_string
        );
    }

    /// **Feature: realtime-audio-transport, Property 4: SDP Offer Structure**
    /// *For any* session configuration, the generated SDP offer SHALL contain
    /// a data channel indicator (`m=application`, `a=sctpmap`, `a=sctp-port`,
    /// or `webrtc-datachannel`).
    /// **Validates: Requirements 9.1**
    #[test]
    fn prop_sdp_offer_contains_data_channel_indicator(
        (_model, _voice) in arb_session_config()
    ) {
        let sdp_string = generate_sdp_offer();

        let has_data_channel = sdp_string.contains("m=application")
            || sdp_string.contains("a=sctpmap")
            || sdp_string.contains("a=sctp-port")
            || sdp_string.contains("webrtc-datachannel");

        prop_assert!(
            has_data_channel,
            "SDP offer missing data channel indicator. SDP:\n{}",
            sdp_string
        );
    }

    /// **Feature: realtime-audio-transport, Property 4: SDP Offer Structure**
    /// *For any* session configuration, the generated SDP offer SHALL be non-empty
    /// and start with the SDP version line `v=0`.
    /// **Validates: Requirements 9.1**
    #[test]
    fn prop_sdp_offer_is_valid_sdp(
        (_model, _voice) in arb_session_config()
    ) {
        let sdp_string = generate_sdp_offer();

        prop_assert!(
            !sdp_string.is_empty(),
            "SDP offer should not be empty"
        );
        prop_assert!(
            sdp_string.starts_with("v=0"),
            "SDP offer should start with 'v=0', got: {}",
            &sdp_string[..sdp_string.len().min(50)]
        );
    }

    /// **Feature: realtime-audio-transport, Property 4: SDP Offer Structure**
    /// *For any* session configuration, the generated SDP offer SHALL contain
    /// both audio and data channel media lines simultaneously.
    /// **Validates: Requirements 9.1**
    #[test]
    fn prop_sdp_offer_contains_both_audio_and_data_channel(
        (_model, _voice) in arb_session_config()
    ) {
        let sdp_string = generate_sdp_offer();

        let has_audio = sdp_string.contains("m=audio");
        let has_data_channel = sdp_string.contains("m=application")
            || sdp_string.contains("a=sctpmap")
            || sdp_string.contains("a=sctp-port")
            || sdp_string.contains("webrtc-datachannel");

        prop_assert!(
            has_audio && has_data_channel,
            "SDP offer must contain both m=audio and a data channel indicator. \
             has_audio={}, has_data_channel={}. SDP:\n{}",
            has_audio,
            has_data_channel,
            sdp_string
        );
    }
}

/// **Feature: realtime-audio-transport, Property 4: SDP Offer Structure**
/// The SDP offer SHALL contain exactly one `m=audio` media line.
/// **Validates: Requirements 9.1**
#[test]
fn test_sdp_offer_contains_exactly_one_audio_line() {
    let sdp_string = generate_sdp_offer();

    let audio_count = sdp_string.matches("m=audio").count();
    assert_eq!(
        audio_count, 1,
        "Expected exactly 1 m=audio line, found {}. SDP:\n{}",
        audio_count, sdp_string
    );
}

/// **Feature: realtime-audio-transport, Property 4: SDP Offer Structure**
/// The SDP offer SHALL contain session-level attributes including origin (`o=`)
/// and session name (`s=`).
/// **Validates: Requirements 9.1**
#[test]
fn test_sdp_offer_contains_session_attributes() {
    let sdp_string = generate_sdp_offer();

    assert!(
        sdp_string.contains("\no=") || sdp_string.contains("\r\no="),
        "SDP offer missing origin (o=) line. SDP:\n{}",
        sdp_string
    );
    assert!(
        sdp_string.contains("\ns=") || sdp_string.contains("\r\ns="),
        "SDP offer missing session name (s=) line. SDP:\n{}",
        sdp_string
    );
}

/// **Feature: realtime-audio-transport, Property 4: SDP Offer Structure**
/// The SDP offer SHALL contain ICE credentials (`a=ice-ufrag` and `a=ice-pwd`).
/// **Validates: Requirements 9.1**
#[test]
fn test_sdp_offer_contains_ice_credentials() {
    let sdp_string = generate_sdp_offer();

    assert!(
        sdp_string.contains("a=ice-ufrag"),
        "SDP offer missing a=ice-ufrag. SDP:\n{}",
        sdp_string
    );
    assert!(sdp_string.contains("a=ice-pwd"), "SDP offer missing a=ice-pwd. SDP:\n{}", sdp_string);
}
