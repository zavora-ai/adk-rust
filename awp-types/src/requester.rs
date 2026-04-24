use serde::{Deserialize, Serialize};

/// Distinguishes between human and agent requesters.
///
/// # Example
///
/// ```
/// use awp_types::RequesterType;
///
/// let rt = RequesterType::Agent;
/// assert_eq!(serde_json::to_string(&rt).unwrap(), "\"agent\"");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequesterType {
    Human,
    Agent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serde_round_trip() {
        for rt in [RequesterType::Human, RequesterType::Agent] {
            let json = serde_json::to_string(&rt).unwrap();
            let deserialized: RequesterType = serde_json::from_str(&json).unwrap();
            assert_eq!(rt, deserialized);
        }
    }

    #[test]
    fn test_serde_lowercase() {
        assert_eq!(serde_json::to_string(&RequesterType::Human).unwrap(), "\"human\"");
        assert_eq!(serde_json::to_string(&RequesterType::Agent).unwrap(), "\"agent\"");
    }
}
