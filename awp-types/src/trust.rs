use serde::{Deserialize, Serialize};
use std::fmt;

/// Classification of the trust relationship with a requester.
///
/// Ordering is defined by discriminant: `Anonymous < Known < Partner < Internal`.
///
/// # Example
///
/// ```
/// use awp_types::TrustLevel;
///
/// assert!(TrustLevel::Anonymous < TrustLevel::Known);
/// assert!(TrustLevel::Known < TrustLevel::Partner);
/// assert!(TrustLevel::Partner < TrustLevel::Internal);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustLevel {
    Anonymous = 0,
    Known = 1,
    Partner = 2,
    Internal = 3,
}

impl fmt::Display for TrustLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrustLevel::Anonymous => write!(f, "anonymous"),
            TrustLevel::Known => write!(f, "known"),
            TrustLevel::Partner => write!(f, "partner"),
            TrustLevel::Internal => write!(f, "internal"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ordering() {
        assert!(TrustLevel::Anonymous < TrustLevel::Known);
        assert!(TrustLevel::Known < TrustLevel::Partner);
        assert!(TrustLevel::Partner < TrustLevel::Internal);
    }

    #[test]
    fn test_display_anonymous() {
        assert_eq!(TrustLevel::Anonymous.to_string(), "anonymous");
    }

    #[test]
    fn test_display_known() {
        assert_eq!(TrustLevel::Known.to_string(), "known");
    }

    #[test]
    fn test_display_partner() {
        assert_eq!(TrustLevel::Partner.to_string(), "partner");
    }

    #[test]
    fn test_display_internal() {
        assert_eq!(TrustLevel::Internal.to_string(), "internal");
    }

    #[test]
    fn test_serde_round_trip() {
        for level in
            [TrustLevel::Anonymous, TrustLevel::Known, TrustLevel::Partner, TrustLevel::Internal]
        {
            let json = serde_json::to_string(&level).unwrap();
            let deserialized: TrustLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(level, deserialized);
        }
    }

    #[test]
    fn test_serde_lowercase() {
        assert_eq!(serde_json::to_string(&TrustLevel::Anonymous).unwrap(), "\"anonymous\"");
        assert_eq!(serde_json::to_string(&TrustLevel::Known).unwrap(), "\"known\"");
        assert_eq!(serde_json::to_string(&TrustLevel::Partner).unwrap(), "\"partner\"");
        assert_eq!(serde_json::to_string(&TrustLevel::Internal).unwrap(), "\"internal\"");
    }
}
