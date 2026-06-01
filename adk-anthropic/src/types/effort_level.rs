use serde::{Deserialize, Serialize};

/// Effort parameter controlling response thoroughness.
/// Passed via `output_config.effort`. GA, no beta header required.
///
/// Supported on Claude Opus 4.8, Opus 4.7, Opus 4.6, Sonnet 4.6, and Opus 4.5.
/// `XHigh` is available on Opus 4.7+. `Max` is available on Opus 4.6+.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffortLevel {
    /// Most efficient — significant token savings.
    Low,
    /// Balanced approach with moderate token savings.
    Medium,
    /// High capability (default). Same as omitting the parameter.
    High,
    /// Very deep reasoning. Opus 4.8 / Opus 4.7+ recommended.
    /// Sits between `High` and `Max` — deeper reasoning than `High`
    /// without the full cost of `Max`.
    XHigh,
    /// Absolute maximum capability. Opus 4.6+ only.
    Max,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization() {
        assert_eq!(serde_json::to_string(&EffortLevel::Low).unwrap(), r#""low""#);
        assert_eq!(serde_json::to_string(&EffortLevel::Medium).unwrap(), r#""medium""#);
        assert_eq!(serde_json::to_string(&EffortLevel::High).unwrap(), r#""high""#);
        assert_eq!(serde_json::to_string(&EffortLevel::XHigh).unwrap(), r#""xhigh""#);
        assert_eq!(serde_json::to_string(&EffortLevel::Max).unwrap(), r#""max""#);
    }

    #[test]
    fn deserialization() {
        let level: EffortLevel = serde_json::from_str(r#""xhigh""#).unwrap();
        assert_eq!(level, EffortLevel::XHigh);

        let level: EffortLevel = serde_json::from_str(r#""max""#).unwrap();
        assert_eq!(level, EffortLevel::Max);
    }
}
