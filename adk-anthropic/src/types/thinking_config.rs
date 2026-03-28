use serde::{Deserialize, Serialize};

/// Controls whether thinking content appears in the response.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingDisplay {
    /// Summarized thinking content (default for Claude 4+ models).
    Summarized,
    /// Thinking content omitted; only signature is populated.
    Omitted,
}

/// Unified thinking configuration covering all modes.
///
/// Uses `#[serde(tag = "type")]` for internally-tagged serialisation.
///
/// # Variants
///
/// - `Enabled` — Budget-based thinking (legacy, deprecated on 4.6 models).
/// - `Adaptive` — Adaptive thinking for Opus 4.6 / Sonnet 4.6.
///   Pair with `output_config.effort` to control reasoning depth.
/// - `Disabled` — Thinking disabled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThinkingConfig {
    /// Budget-based thinking (deprecated on 4.6 models).
    Enabled {
        /// Token budget for thinking. Must be ≥1024 and less than `max_tokens`.
        budget_tokens: u32,
        /// Optional display control.
        #[serde(skip_serializing_if = "Option::is_none")]
        display: Option<ThinkingDisplay>,
    },
    /// Adaptive thinking for Opus 4.6 / Sonnet 4.6.
    /// Use `output_config.effort` to control reasoning depth.
    Adaptive {
        /// Optional display control.
        #[serde(skip_serializing_if = "Option::is_none")]
        display: Option<ThinkingDisplay>,
    },
    /// Thinking disabled.
    Disabled,
}

impl ThinkingConfig {
    /// Returns the number of budget tokens configured for thinking.
    /// Returns 0 if thinking is disabled or adaptive.
    pub fn num_tokens(&self) -> u32 {
        match self {
            ThinkingConfig::Enabled { budget_tokens, .. } => *budget_tokens,
            ThinkingConfig::Adaptive { .. } | ThinkingConfig::Disabled => 0,
        }
    }

    /// Create a budget-based thinking configuration (legacy).
    pub fn enabled(budget_tokens: u32) -> Self {
        Self::Enabled { budget_tokens, display: None }
    }

    /// Create an adaptive thinking configuration.
    /// Pair with `output_config.effort` to control reasoning depth.
    pub fn adaptive() -> Self {
        Self::Adaptive { display: None }
    }

    /// Create a disabled thinking configuration.
    pub fn disabled() -> Self {
        Self::Disabled
    }
}

impl Default for ThinkingConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn enabled_serialization() {
        let config = ThinkingConfig::enabled(2048);
        assert_eq!(to_value(config).unwrap(), json!({"type": "enabled", "budget_tokens": 2048}));
    }

    #[test]
    fn enabled_with_display() {
        let config = ThinkingConfig::Enabled {
            budget_tokens: 4096,
            display: Some(ThinkingDisplay::Omitted),
        };
        assert_eq!(
            to_value(config).unwrap(),
            json!({"type": "enabled", "budget_tokens": 4096, "display": "omitted"})
        );
    }

    #[test]
    fn adaptive_serialization() {
        let config = ThinkingConfig::adaptive();
        assert_eq!(to_value(config).unwrap(), json!({"type": "adaptive"}));
    }

    #[test]
    fn adaptive_with_display() {
        let config = ThinkingConfig::Adaptive { display: Some(ThinkingDisplay::Omitted) };
        assert_eq!(to_value(config).unwrap(), json!({"type": "adaptive", "display": "omitted"}));
    }

    #[test]
    fn disabled_serialization() {
        assert_eq!(to_value(ThinkingConfig::disabled()).unwrap(), json!({"type": "disabled"}));
    }

    #[test]
    fn enabled_deserialization() {
        let config: ThinkingConfig =
            serde_json::from_value(json!({"type": "enabled", "budget_tokens": 2048})).unwrap();
        assert_eq!(config, ThinkingConfig::Enabled { budget_tokens: 2048, display: None });
    }

    #[test]
    fn adaptive_deserialization() {
        let config: ThinkingConfig =
            serde_json::from_value(json!({"type": "adaptive", "display": "omitted"})).unwrap();
        assert_eq!(config, ThinkingConfig::Adaptive { display: Some(ThinkingDisplay::Omitted) });
    }

    #[test]
    fn disabled_deserialization() {
        let config: ThinkingConfig = serde_json::from_value(json!({"type": "disabled"})).unwrap();
        assert_eq!(config, ThinkingConfig::Disabled);
    }

    #[test]
    fn num_tokens() {
        assert_eq!(ThinkingConfig::enabled(8192).num_tokens(), 8192);
        assert_eq!(ThinkingConfig::adaptive().num_tokens(), 0);
        assert_eq!(ThinkingConfig::disabled().num_tokens(), 0);
    }
}
