//! Property-based tests for Vertex AI Live URL construction.
//!
//! **Feature: realtime-audio-transport, Property 1: Vertex AI Live URL Construction**
//! *For any* non-empty region string containing only alphanumeric characters and hyphens,
//! and any non-empty project ID string, the `build_vertex_live_url` function SHALL produce
//! a URL that:
//! - starts with `wss://`
//! - contains `{region}-aiplatform.googleapis.com` as the host
//! - contains the path `/ws/google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent`
//! - contains `project_id={project_id}` as a query parameter
//! - is parseable as a valid URL
//!
//! Additionally, empty region or empty project_id inputs SHALL return a `ConfigError`.
//! **Validates: Requirements 1.2, 3.1, 3.2, 3.3, 3.4**

#![cfg(feature = "vertex-live")]

use adk_realtime::gemini::build_vertex_live_url;
use proptest::prelude::*;

/// Generator for valid GCP region strings: `[a-z0-9][a-z0-9-]{0,20}[a-z0-9]`
fn arb_region() -> impl Strategy<Value = String> {
    "[a-z0-9][a-z0-9\\-]{0,20}[a-z0-9]"
}

/// Generator for valid GCP project ID strings: `[a-z][a-z0-9-]{4,28}[a-z0-9]`
fn arb_project_id() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9\\-]{4,28}[a-z0-9]"
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: realtime-audio-transport, Property 1: Vertex AI Live URL Construction**
    /// *For any* valid region and project_id, the URL SHALL start with `wss://`.
    /// **Validates: Requirements 1.2, 3.1, 3.2**
    #[test]
    fn prop_vertex_url_starts_with_wss(
        region in arb_region(),
        project_id in arb_project_id(),
    ) {
        let url = build_vertex_live_url(&region, &project_id)
            .expect("should produce a valid URL for non-empty inputs");
        prop_assert!(
            url.starts_with("wss://"),
            "URL '{}' does not start with 'wss://'",
            url
        );
    }

    /// **Feature: realtime-audio-transport, Property 1: Vertex AI Live URL Construction**
    /// *For any* valid region and project_id, the URL SHALL contain
    /// `{region}-aiplatform.googleapis.com` as the host.
    /// **Validates: Requirements 1.2, 3.1, 3.2**
    #[test]
    fn prop_vertex_url_contains_regional_host(
        region in arb_region(),
        project_id in arb_project_id(),
    ) {
        let url = build_vertex_live_url(&region, &project_id)
            .expect("should produce a valid URL for non-empty inputs");
        let expected_host = format!("{}-aiplatform.googleapis.com", region);
        prop_assert!(
            url.contains(&expected_host),
            "URL '{}' does not contain expected host '{}'",
            url,
            expected_host
        );
    }

    /// **Feature: realtime-audio-transport, Property 1: Vertex AI Live URL Construction**
    /// *For any* valid region and project_id, the URL SHALL contain the correct API path.
    /// **Validates: Requirements 1.2, 3.1**
    #[test]
    fn prop_vertex_url_contains_bidi_path(
        region in arb_region(),
        project_id in arb_project_id(),
    ) {
        let url = build_vertex_live_url(&region, &project_id)
            .expect("should produce a valid URL for non-empty inputs");
        let expected_path = "/ws/google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent";
        prop_assert!(
            url.contains(expected_path),
            "URL '{}' does not contain expected path '{}'",
            url,
            expected_path
        );
    }

    /// **Feature: realtime-audio-transport, Property 1: Vertex AI Live URL Construction**
    /// *For any* valid region and project_id, the URL SHALL contain
    /// `project_id={project_id}` as a query parameter.
    /// **Validates: Requirements 3.1**
    #[test]
    fn prop_vertex_url_contains_project_id_param(
        region in arb_region(),
        project_id in arb_project_id(),
    ) {
        let url = build_vertex_live_url(&region, &project_id)
            .expect("should produce a valid URL for non-empty inputs");
        let expected_param = format!("project_id={}", project_id);
        prop_assert!(
            url.contains(&expected_param),
            "URL '{}' does not contain expected query param '{}'",
            url,
            expected_param
        );
    }

    /// **Feature: realtime-audio-transport, Property 1: Vertex AI Live URL Construction**
    /// *For any* valid region and project_id, the URL SHALL be parseable as a valid URL.
    /// **Validates: Requirements 3.2**
    #[test]
    fn prop_vertex_url_is_parseable(
        region in arb_region(),
        project_id in arb_project_id(),
    ) {
        let url = build_vertex_live_url(&region, &project_id)
            .expect("should produce a valid URL for non-empty inputs");
        let parsed = url::Url::parse(&url);
        prop_assert!(
            parsed.is_ok(),
            "URL '{}' is not parseable: {:?}",
            url,
            parsed.err()
        );
    }

    /// **Feature: realtime-audio-transport, Property 1: Vertex AI Live URL Construction**
    /// *For any* valid project_id, an empty region SHALL return a ConfigError.
    /// **Validates: Requirements 3.3**
    #[test]
    fn prop_empty_region_returns_config_error(
        project_id in arb_project_id(),
    ) {
        let result = build_vertex_live_url("", &project_id);
        prop_assert!(
            result.is_err(),
            "Expected ConfigError for empty region, got Ok({})",
            result.unwrap()
        );
        let err = result.unwrap_err();
        let display = format!("{}", err);
        prop_assert!(
            display.contains("region"),
            "Error message '{}' should mention 'region'",
            display
        );
    }

    /// **Feature: realtime-audio-transport, Property 1: Vertex AI Live URL Construction**
    /// *For any* valid region, an empty project_id SHALL return a ConfigError.
    /// **Validates: Requirements 3.4**
    #[test]
    fn prop_empty_project_id_returns_config_error(
        region in arb_region(),
    ) {
        let result = build_vertex_live_url(&region, "");
        prop_assert!(
            result.is_err(),
            "Expected ConfigError for empty project_id, got Ok({})",
            result.unwrap()
        );
        let err = result.unwrap_err();
        let display = format!("{}", err);
        prop_assert!(
            display.contains("project_id"),
            "Error message '{}' should mention 'project_id'",
            display
        );
    }
}
