//! Property-based tests for Vertex AI Live URL construction.
//!
//! **Feature: realtime-audio-transport, Property 1: Vertex AI Live URL Construction**
//! *For any* non-empty region string containing only alphanumeric characters and hyphens,
//! the `build_vertex_live_url` function SHALL produce a URL that:
//! - starts with `wss://`
//! - contains `{region}-aiplatform.googleapis.com` as the host
//! - contains the path `/ws/google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent`
//! - is parseable as a valid URL
//!
//! Additionally, empty region inputs SHALL return a `ConfigError`.
//! **Validates: Requirements 1.2, 3.1, 3.2, 3.3**

#![cfg(feature = "vertex-live")]

use adk_realtime::gemini::build_vertex_live_url;
use proptest::prelude::*;

/// Generator for valid GCP region strings: `[a-z0-9][a-z0-9-]{0,20}[a-z0-9]`
fn arb_region() -> impl Strategy<Value = String> {
    "[a-z0-9][a-z0-9\\-]{0,20}[a-z0-9]"
}

proptest! {
    fn prop_vertex_url_starts_with_wss(region in arb_region()) {
        let url = build_vertex_live_url(&region)
            .expect("should produce a valid URL for non-empty inputs");
        prop_assert!(
            url.starts_with("wss://"),
            "URL '{}' does not start with 'wss://'",
            url
        );
    }

    fn prop_vertex_url_contains_regional_host(region in arb_region()) {
        let url = build_vertex_live_url(&region)
            .expect("should produce a valid URL for non-empty inputs");
        let expected_host = format!("{}-aiplatform.googleapis.com", region);
        prop_assert!(
            url.contains(&expected_host),
            "URL '{}' does not contain expected host '{}'",
            url,
            expected_host
        );
    }

    fn prop_vertex_url_contains_bidi_path(region in arb_region()) {
        let url = build_vertex_live_url(&region)
            .expect("should produce a valid URL for non-empty inputs");
        let expected_path = "/ws/google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent";
        prop_assert!(
            url.contains(expected_path),
            "URL '{}' does not contain expected path '{}'",
            url,
            expected_path
        );
    }

    fn prop_vertex_url_is_parseable(region in arb_region()) {
        let url = build_vertex_live_url(&region)
            .expect("should produce a valid URL for non-empty inputs");
        let parsed = url::Url::parse(&url);
        prop_assert!(
            parsed.is_ok(),
            "URL '{}' is not parseable: {:?}",
            url,
            parsed.err()
        );
    }

}

#[test]
fn prop_empty_region_returns_config_error() {
    let result = build_vertex_live_url("");
    assert!(result.is_err(), "Expected ConfigError for empty region, got Ok({})", result.unwrap());
    let err = result.unwrap_err();
    let display = format!("{}", err);
    assert!(display.contains("region"), "Error message '{}' should mention 'region'", display);
}
