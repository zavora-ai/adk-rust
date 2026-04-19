#![cfg(feature = "video-avatar")]
//! Property-based tests for Avatar Config.
//!
//! **Feature: competitive-parity-v070, Property 11: Avatar Config JSON Round-Trip**
//! *For any* valid `AvatarConfig` (with arbitrary source URL, optional lip-sync,
//! optional rendering), serializing to JSON and deserializing back produces an
//! equivalent `AvatarConfig`.
//! **Validates: Requirements 11.1, 11.5**

use adk_realtime::avatar::{AvatarConfig, AvatarProviderKind, LipSyncConfig, RenderingConfig};
use proptest::prelude::*;

/// Generate an arbitrary URL-like string for the avatar source.
fn arb_source_url() -> impl Strategy<Value = String> {
    prop_oneof![
        "https://[a-z]{3,12}\\.[a-z]{2,6}/[a-z0-9_-]{1,20}\\.(mp4|png|jpg|webm)",
        "https://cdn\\.[a-z]{3,10}\\.com/avatars/[a-z0-9]{4,16}",
        "[a-z]{5,30}",
    ]
}

/// Generate an arbitrary sync mode string.
fn arb_sync_mode() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("viseme".to_string()),
        Just("phoneme".to_string()),
        Just("blend_shape".to_string()),
        "[a-z_]{3,15}",
    ]
}

/// Generate an arbitrary LipSyncConfig.
fn arb_lip_sync_config() -> impl Strategy<Value = LipSyncConfig> {
    (any::<bool>(), proptest::option::of(arb_sync_mode()))
        .prop_map(|(enabled, sync_mode)| LipSyncConfig { enabled, sync_mode })
}

/// Generate an arbitrary resolution string.
fn arb_resolution() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("360p".to_string()),
        Just("480p".to_string()),
        Just("720p".to_string()),
        Just("1080p".to_string()),
        Just("4k".to_string()),
        "[0-9]{3,4}x[0-9]{3,4}",
    ]
}

/// Generate an arbitrary RenderingConfig.
fn arb_rendering_config() -> impl Strategy<Value = RenderingConfig> {
    (proptest::option::of(arb_resolution()), proptest::option::of(1u32..120))
        .prop_map(|(resolution, frame_rate)| RenderingConfig { resolution, frame_rate })
}

/// Generate an arbitrary AvatarProviderKind.
fn arb_provider_kind() -> impl Strategy<Value = AvatarProviderKind> {
    prop_oneof![Just(AvatarProviderKind::HeyGen), Just(AvatarProviderKind::DId),]
}

/// Generate an arbitrary AvatarConfig.
fn arb_avatar_config() -> impl Strategy<Value = AvatarConfig> {
    (
        arb_source_url(),
        proptest::option::of(arb_lip_sync_config()),
        proptest::option::of(arb_rendering_config()),
        proptest::option::of(arb_provider_kind()),
    )
        .prop_map(|(source_url, lip_sync, rendering, provider)| AvatarConfig {
            source_url,
            lip_sync,
            rendering,
            provider,
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// **Feature: competitive-parity-v070, Property 11: Avatar Config JSON Round-Trip**
    /// *For any* valid AvatarConfig, serializing to JSON and deserializing back
    /// produces an equivalent AvatarConfig.
    /// **Validates: Requirements 11.1, 11.5**
    #[test]
    fn prop_avatar_config_json_round_trip(config in arb_avatar_config()) {
        let json = serde_json::to_string(&config).expect("serialization should succeed");
        let deserialized: AvatarConfig = serde_json::from_str(&json).expect("deserialization should succeed");
        prop_assert_eq!(&config, &deserialized);
    }

    /// Round-trip through serde_json::Value preserves equivalence.
    #[test]
    fn prop_avatar_config_value_round_trip(config in arb_avatar_config()) {
        let value = serde_json::to_value(&config).expect("to_value should succeed");
        let deserialized: AvatarConfig = serde_json::from_value(value).expect("from_value should succeed");
        prop_assert_eq!(&config, &deserialized);
    }

    /// Serialized JSON uses camelCase field names per serde configuration.
    #[test]
    fn prop_avatar_config_uses_camel_case(config in arb_avatar_config()) {
        let json = serde_json::to_string(&config).expect("serialization should succeed");
        // sourceUrl is always present
        prop_assert!(json.contains("sourceUrl"), "expected camelCase 'sourceUrl' in JSON: {json}");
        prop_assert!(!json.contains("source_url"), "unexpected snake_case 'source_url' in JSON: {json}");

        if config.lip_sync.is_some() {
            prop_assert!(json.contains("lipSync"), "expected camelCase 'lipSync' in JSON: {json}");
        }
        if config.rendering.is_some() {
            if config.rendering.as_ref().unwrap().frame_rate.is_some() {
                prop_assert!(json.contains("frameRate"), "expected camelCase 'frameRate' in JSON: {json}");
            }
        }
    }
}
