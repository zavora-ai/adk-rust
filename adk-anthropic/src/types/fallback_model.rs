use serde::{Deserialize, Serialize};

/// A fallback model the server may substitute when the requested model is
/// unavailable. Requires beta header `server-side-fallback-2026-07-01`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackModel {
    /// The model to fall back to.
    pub model: String,
}

impl FallbackModel {
    /// Create a new fallback model entry.
    pub fn new(model: impl Into<String>) -> Self {
        Self { model: model.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization() {
        let fallback = FallbackModel::new("claude-sonnet-4-6");
        assert_eq!(serde_json::to_string(&fallback).unwrap(), r#"{"model":"claude-sonnet-4-6"}"#);
    }

    #[test]
    fn deserialization() {
        let fallback: FallbackModel =
            serde_json::from_str(r#"{"model":"claude-sonnet-4-6"}"#).unwrap();
        assert_eq!(fallback, FallbackModel::new("claude-sonnet-4-6"));
    }
}
