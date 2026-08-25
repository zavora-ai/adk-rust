//! Precedence tests for `provider_from_env` — the Vertex opt-in flags
//! (`GOOGLE_GENAI_USE_ENTERPRISE` / `GOOGLE_GENAI_USE_VERTEXAI`) come before
//! API-key sniffing.
//!
//! Environment variables are process-global, so every test takes `ENV_LOCK`
//! through [`EnvGuard`], which clears the relevant variables, applies the
//! test's values, and restores the previous state on drop.
#![cfg(feature = "gemini")]

use adk_rust::{model::catalog::GEMINI_DEFAULT, provider_from_env};
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Every variable provider detection (or the ADC credential builder) consults.
const VARS: &[&str] = &[
    "GOOGLE_GENAI_USE_ENTERPRISE",
    "GOOGLE_GENAI_USE_VERTEXAI",
    "GOOGLE_CLOUD_PROJECT",
    "GOOGLE_CLOUD_LOCATION",
    "GOOGLE_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
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
fn no_flags_no_keys_errors() {
    let _guard = EnvGuard::new(&[]);
    assert!(provider_from_env().is_err());
}

#[test]
fn no_flags_google_api_key_selects_gemini() {
    let _guard = EnvGuard::new(&[("GOOGLE_API_KEY", "test-key")]);
    let model = provider_from_env().expect("expected the gemini studio provider");
    assert_eq!(model.name(), GEMINI_DEFAULT);
}

#[cfg(feature = "gemini-vertex")]
mod vertex_enabled {
    use super::*;

    // The ADC credential builder registers a token-cache task, so a Tokio
    // runtime must be current when the Vertex client is constructed.
    #[tokio::test]
    async fn vertex_flag_selects_vertex_without_any_api_key() {
        // No API key set: only the Vertex path can construct a provider, so an
        // Ok result proves the flag routed detection to Vertex AI.
        let _guard = EnvGuard::new(&[
            ("GOOGLE_GENAI_USE_ENTERPRISE", "true"),
            ("GOOGLE_CLOUD_PROJECT", "test-project"),
            ("GOOGLE_CLOUD_LOCATION", "us-central1"),
        ]);
        let model = provider_from_env().expect("expected the vertex provider");
        assert_eq!(model.name(), GEMINI_DEFAULT);
    }

    #[test]
    fn vertex_flag_beats_api_keys_and_incomplete_config_errors() {
        // A truthy flag with missing project/location errors even though
        // GOOGLE_API_KEY (and, when compiled, ANTHROPIC_API_KEY /
        // OPENAI_API_KEY) would have produced a provider — proof the flags are
        // consulted first and never silently fall back to Studio.
        let _guard = EnvGuard::new(&[
            ("GOOGLE_GENAI_USE_VERTEXAI", "1"),
            ("GOOGLE_API_KEY", "test-key"),
            ("ANTHROPIC_API_KEY", "test-key"),
            ("OPENAI_API_KEY", "test-key"),
        ]);
        assert!(provider_from_env().is_err());
    }

    #[test]
    fn falsy_enterprise_flag_overrides_truthy_vertexai_flag() {
        // GOOGLE_GENAI_USE_ENTERPRISE takes precedence when both are set.
        let _guard = EnvGuard::new(&[
            ("GOOGLE_GENAI_USE_ENTERPRISE", "0"),
            ("GOOGLE_GENAI_USE_VERTEXAI", "true"),
            ("GOOGLE_API_KEY", "test-key"),
        ]);
        let model = provider_from_env().expect("expected the gemini studio provider");
        assert_eq!(model.name(), GEMINI_DEFAULT);
    }
}

#[cfg(not(feature = "gemini-vertex"))]
mod vertex_disabled {
    use super::*;

    #[test]
    fn vertex_flag_without_feature_warns_and_falls_back_to_api_keys() {
        let _guard = EnvGuard::new(&[
            ("GOOGLE_GENAI_USE_ENTERPRISE", "true"),
            ("GOOGLE_API_KEY", "test-key"),
        ]);
        // The residual Studio path: flag set, feature missing → warn + fall
        // through to API-key detection.
        let model = provider_from_env().expect("expected the api-key fallback provider");
        assert_eq!(model.name(), GEMINI_DEFAULT);
    }

    #[test]
    fn vertex_flag_without_feature_and_no_keys_errors() {
        let _guard = EnvGuard::new(&[("GOOGLE_GENAI_USE_ENTERPRISE", "true")]);
        assert!(provider_from_env().is_err());
    }
}
