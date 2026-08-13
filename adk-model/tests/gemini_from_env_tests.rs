//! Environment-driven construction tests for `GeminiModel::from_env` and
//! `vertex_env_requested`.
//!
//! Environment variables are process-global, so every test takes `ENV_LOCK`
//! through [`EnvGuard`], which clears the relevant variables, applies the
//! test's values, and restores the previous state on drop. This keeps the
//! suite correct under `cargo test` (threads share one process); `cargo
//! nextest` isolates each test in its own process anyway.
#![cfg(feature = "gemini")]

use adk_model::gemini::{GeminiModel, vertex_env_requested};
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Every variable `from_env` (or the ADC credential builder) consults.
const VARS: &[&str] = &[
    "GOOGLE_GENAI_USE_ENTERPRISE",
    "GOOGLE_GENAI_USE_VERTEXAI",
    "GOOGLE_CLOUD_PROJECT",
    "GOOGLE_CLOUD_LOCATION",
    "GOOGLE_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
];

/// Serializes env access, applies `vars` on a clean slate, restores on drop.
struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn new(vars: &[(&str, &str)]) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = VARS.iter().map(|&name| (name, std::env::var(name).ok())).collect();
        for &name in VARS {
            // SAFETY: serialized by ENV_LOCK; test-only.
            unsafe { std::env::remove_var(name) };
        }
        for &(name, value) in vars {
            // SAFETY: serialized by ENV_LOCK; test-only.
            unsafe { std::env::set_var(name, value) };
        }
        Self { _lock: lock, saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in &self.saved {
            match value {
                // SAFETY: serialized by ENV_LOCK; test-only.
                Some(v) => unsafe { std::env::set_var(name, v) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
    }
}

#[test]
fn enterprise_flag_alone_requests_vertex() {
    let _guard = EnvGuard::new(&[("GOOGLE_GENAI_USE_ENTERPRISE", "1")]);
    assert!(vertex_env_requested());
}

#[test]
fn vertexai_flag_alone_requests_vertex() {
    let _guard = EnvGuard::new(&[("GOOGLE_GENAI_USE_VERTEXAI", "true")]);
    assert!(vertex_env_requested());
}

#[test]
fn flag_values_are_one_or_case_insensitive_true() {
    for (value, expected) in
        [("1", true), ("true", true), ("TRUE", true), ("True", true), ("0", false), ("yes", false)]
    {
        let _guard = EnvGuard::new(&[("GOOGLE_GENAI_USE_ENTERPRISE", value)]);
        assert_eq!(vertex_env_requested(), expected, "value {value:?}");
    }
}

#[test]
fn enterprise_wins_when_both_flags_set() {
    // Truthy enterprise + falsy vertexai → vertex.
    let guard = EnvGuard::new(&[
        ("GOOGLE_GENAI_USE_ENTERPRISE", "true"),
        ("GOOGLE_GENAI_USE_VERTEXAI", "false"),
    ]);
    assert!(vertex_env_requested());
    drop(guard);

    // Falsy enterprise overrides truthy vertexai — enterprise takes precedence.
    let _guard = EnvGuard::new(&[
        ("GOOGLE_GENAI_USE_ENTERPRISE", "0"),
        ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
    ]);
    assert!(!vertex_env_requested());
}

#[test]
fn no_flags_means_studio() {
    let _guard = EnvGuard::new(&[]);
    assert!(!vertex_env_requested());
}

#[test]
fn no_flags_uses_google_api_key() {
    let _guard = EnvGuard::new(&[("GOOGLE_API_KEY", "test-key")]);
    let result = GeminiModel::from_env("gemini-2.5-flash");
    assert!(result.is_ok(), "expected studio construction to succeed: {:?}", result.err());
}

#[test]
fn no_flags_falls_back_to_gemini_api_key() {
    let _guard = EnvGuard::new(&[("GEMINI_API_KEY", "test-key")]);
    let result = GeminiModel::from_env("gemini-2.5-flash");
    assert!(result.is_ok(), "expected studio construction to succeed: {:?}", result.err());
}

#[test]
fn no_flags_no_keys_errors_naming_the_variables() {
    let _guard = EnvGuard::new(&[]);
    let err = GeminiModel::from_env("gemini-2.5-flash").err().expect("expected an error");
    let msg = err.to_string();
    assert!(msg.contains("GOOGLE_API_KEY"), "message should name GOOGLE_API_KEY: {msg}");
    assert!(msg.contains("GEMINI_API_KEY"), "message should name GEMINI_API_KEY: {msg}");
}

#[cfg(feature = "gemini-vertex")]
mod vertex_enabled {
    use super::*;

    // The ADC credential builder registers a token-cache task, so a Tokio
    // runtime must be current when the Vertex client is constructed.
    #[tokio::test]
    async fn enterprise_flag_builds_vertex_client_via_adc() {
        let _guard = EnvGuard::new(&[
            ("GOOGLE_GENAI_USE_ENTERPRISE", "1"),
            ("GOOGLE_CLOUD_PROJECT", "test-project"),
            ("GOOGLE_CLOUD_LOCATION", "us-central1"),
        ]);
        let result = GeminiModel::from_env("gemini-2.5-flash");
        assert!(result.is_ok(), "expected vertex construction to succeed: {:?}", result.err());
    }

    #[tokio::test]
    async fn vertexai_flag_builds_vertex_client_via_adc() {
        let _guard = EnvGuard::new(&[
            ("GOOGLE_GENAI_USE_VERTEXAI", "TRUE"),
            ("GOOGLE_CLOUD_PROJECT", "test-project"),
            ("GOOGLE_CLOUD_LOCATION", "us-central1"),
        ]);
        let result = GeminiModel::from_env("gemini-2.5-flash");
        assert!(result.is_ok(), "expected vertex construction to succeed: {:?}", result.err());
    }

    #[test]
    fn missing_project_and_location_errors_naming_both() {
        let _guard = EnvGuard::new(&[("GOOGLE_GENAI_USE_ENTERPRISE", "true")]);
        let err = GeminiModel::from_env("gemini-2.5-flash").err().expect("expected an error");
        let msg = err.to_string();
        assert!(msg.contains("GOOGLE_CLOUD_PROJECT"), "should name GOOGLE_CLOUD_PROJECT: {msg}");
        assert!(msg.contains("GOOGLE_CLOUD_LOCATION"), "should name GOOGLE_CLOUD_LOCATION: {msg}");
    }

    #[test]
    fn missing_location_errors_naming_only_location() {
        let _guard = EnvGuard::new(&[
            ("GOOGLE_GENAI_USE_ENTERPRISE", "true"),
            ("GOOGLE_CLOUD_PROJECT", "test-project"),
        ]);
        let err = GeminiModel::from_env("gemini-2.5-flash").err().expect("expected an error");
        let msg = err.to_string();
        assert!(msg.contains("GOOGLE_CLOUD_LOCATION"), "should name GOOGLE_CLOUD_LOCATION: {msg}");
        assert!(
            !msg.contains("GOOGLE_CLOUD_PROJECT not set")
                && !msg.contains("GOOGLE_CLOUD_PROJECT and"),
            "should not report GOOGLE_CLOUD_PROJECT as missing: {msg}"
        );
    }

    #[test]
    fn flag_takes_precedence_over_api_key() {
        // A truthy flag with incomplete Vertex config errors instead of
        // silently falling back to the Studio endpoint the API key would reach.
        let _guard = EnvGuard::new(&[
            ("GOOGLE_GENAI_USE_ENTERPRISE", "true"),
            ("GOOGLE_API_KEY", "test-key"),
        ]);
        assert!(GeminiModel::from_env("gemini-2.5-flash").is_err());
    }
}

#[cfg(not(feature = "gemini-vertex"))]
mod vertex_disabled {
    use super::*;

    #[test]
    fn flag_without_feature_errors_pointing_at_the_feature() {
        let _guard = EnvGuard::new(&[
            ("GOOGLE_GENAI_USE_ENTERPRISE", "1"),
            ("GOOGLE_CLOUD_PROJECT", "test-project"),
            ("GOOGLE_CLOUD_LOCATION", "us-central1"),
            // The API key must not rescue the call — no silent Studio fallback.
            ("GOOGLE_API_KEY", "test-key"),
        ]);
        let err = GeminiModel::from_env("gemini-2.5-flash").err().expect("expected an error");
        let msg = err.to_string();
        assert!(msg.contains("gemini-vertex"), "should point at the gemini-vertex feature: {msg}");
    }
}
