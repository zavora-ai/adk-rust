use serde::{Deserialize, Serialize};

/// Speed mode for latency-critical workloads (research preview).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpeedMode {
    /// Fast mode for reduced latency.
    Fast,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization() {
        assert_eq!(serde_json::to_string(&SpeedMode::Fast).unwrap(), r#""fast""#);
    }

    #[test]
    fn deserialization() {
        let mode: SpeedMode = serde_json::from_str(r#""fast""#).unwrap();
        assert_eq!(mode, SpeedMode::Fast);
    }
}
