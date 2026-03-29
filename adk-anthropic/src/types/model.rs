use serde::{Deserialize, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

/// Represents an Anthropic model identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Model {
    /// Known model versions
    Known(KnownModel),
    /// Custom model identifier (for future models or private models)
    Custom(String),
}

/// Known Anthropic model versions.
///
/// Covers the current generation (4.6), previous generation (4.5),
/// and legacy 4.0/4.1 models. Any string not matching a known variant
/// deserialises into `Model::Custom`.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum KnownModel {
    // 4.6 (latest)
    /// Claude Opus 4.6
    ClaudeOpus46,
    /// Claude Sonnet 4.6
    ClaudeSonnet46,

    // 4.5
    /// Claude Opus 4.5 (alias)
    ClaudeOpus45,
    /// Claude Opus 4.5 (2025-11-01 snapshot)
    ClaudeOpus4520251101,
    /// Claude Sonnet 4.5 (alias)
    ClaudeSonnet45,
    /// Claude Sonnet 4.5 (2025-09-29 snapshot)
    ClaudeSonnet4520250929,
    /// Claude Haiku 4.5 (alias)
    ClaudeHaiku45,
    /// Claude Haiku 4.5 (2025-10-01 snapshot)
    ClaudeHaiku4520251001,

    // 4.0 / 4.1 (legacy)
    /// Claude Sonnet 4 (alias)
    ClaudeSonnet4,
    /// Claude Sonnet 4 (2025-05-14 snapshot)
    ClaudeSonnet420250514,
    /// Claude Opus 4 (alias)
    ClaudeOpus4,
    /// Claude Opus 4 (2025-05-14 snapshot)
    ClaudeOpus420250514,
    /// Claude Opus 4.1 (2025-08-05 snapshot)
    ClaudeOpus4120250805,
}

impl KnownModel {
    /// Returns the wire-format string for this model.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ClaudeOpus46 => "claude-opus-4-6",
            Self::ClaudeSonnet46 => "claude-sonnet-4-6",
            Self::ClaudeOpus45 => "claude-opus-4-5",
            Self::ClaudeOpus4520251101 => "claude-opus-4-5-20251101",
            Self::ClaudeSonnet45 => "claude-sonnet-4-5",
            Self::ClaudeSonnet4520250929 => "claude-sonnet-4-5-20250929",
            Self::ClaudeHaiku45 => "claude-haiku-4-5",
            Self::ClaudeHaiku4520251001 => "claude-haiku-4-5-20251001",
            Self::ClaudeSonnet4 => "claude-sonnet-4-0",
            Self::ClaudeSonnet420250514 => "claude-sonnet-4-20250514",
            Self::ClaudeOpus4 => "claude-opus-4-0",
            Self::ClaudeOpus420250514 => "claude-opus-4-20250514",
            Self::ClaudeOpus4120250805 => "claude-opus-4-1-20250805",
        }
    }
}

impl fmt::Display for Model {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Model::Known(k) => write!(f, "{}", k.as_str()),
            Model::Custom(c) => write!(f, "{c}"),
        }
    }
}

impl fmt::Display for KnownModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl Serialize for Model {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Model {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match KnownModel::from_str(&s) {
            Ok(known) => Ok(Model::Known(known)),
            Err(()) => Ok(Model::Custom(s)),
        }
    }
}

impl From<KnownModel> for Model {
    fn from(model: KnownModel) -> Self {
        Model::Known(model)
    }
}

impl FromStr for KnownModel {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude-opus-4-6" => Ok(Self::ClaudeOpus46),
            "claude-sonnet-4-6" => Ok(Self::ClaudeSonnet46),
            "claude-opus-4-5" => Ok(Self::ClaudeOpus45),
            "claude-opus-4-5-20251101" => Ok(Self::ClaudeOpus4520251101),
            "claude-sonnet-4-5" => Ok(Self::ClaudeSonnet45),
            "claude-sonnet-4-5-20250929" => Ok(Self::ClaudeSonnet4520250929),
            "claude-haiku-4-5" => Ok(Self::ClaudeHaiku45),
            "claude-haiku-4-5-20251001" => Ok(Self::ClaudeHaiku4520251001),
            "claude-sonnet-4-0" => Ok(Self::ClaudeSonnet4),
            "claude-sonnet-4-20250514" => Ok(Self::ClaudeSonnet420250514),
            "claude-opus-4-0" => Ok(Self::ClaudeOpus4),
            "claude-opus-4-20250514" => Ok(Self::ClaudeOpus420250514),
            "claude-opus-4-1-20250805" => Ok(Self::ClaudeOpus4120250805),
            _ => Err(()),
        }
    }
}

impl FromStr for Model {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match KnownModel::from_str(s) {
            Ok(known) => Ok(Model::Known(known)),
            Err(()) => Ok(Model::Custom(s.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_models_roundtrip() {
        for (variant, wire) in [
            (KnownModel::ClaudeOpus46, "claude-opus-4-6"),
            (KnownModel::ClaudeSonnet46, "claude-sonnet-4-6"),
            (KnownModel::ClaudeHaiku45, "claude-haiku-4-5"),
        ] {
            let model = Model::Known(variant);
            let json = serde_json::to_string(&model).unwrap();
            assert_eq!(json, format!("\"{wire}\""));
            let back: Model = serde_json::from_str(&json).unwrap();
            assert_eq!(back, model);
        }
    }

    #[test]
    fn legacy_models_roundtrip() {
        for (variant, wire) in [
            (KnownModel::ClaudeOpus45, "claude-opus-4-5"),
            (KnownModel::ClaudeSonnet45, "claude-sonnet-4-5"),
            (KnownModel::ClaudeSonnet4, "claude-sonnet-4-0"),
            (KnownModel::ClaudeOpus4, "claude-opus-4-0"),
            (KnownModel::ClaudeOpus4120250805, "claude-opus-4-1-20250805"),
        ] {
            let model = Model::Known(variant);
            let json = serde_json::to_string(&model).unwrap();
            assert_eq!(json, format!("\"{wire}\""));
            let back: Model = serde_json::from_str(&json).unwrap();
            assert_eq!(back, model);
        }
    }

    #[test]
    fn unknown_string_becomes_custom() {
        let json = r#""claude-99-turbo""#;
        let model: Model = serde_json::from_str(json).unwrap();
        assert_eq!(model, Model::Custom("claude-99-turbo".to_string()));
    }

    #[test]
    fn display() {
        assert_eq!(Model::Known(KnownModel::ClaudeSonnet46).to_string(), "claude-sonnet-4-6");
        assert_eq!(Model::Custom("x".to_string()).to_string(), "x");
    }
}
