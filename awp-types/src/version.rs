use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// AWP protocol version with major and minor components.
///
/// Versions are compatible when their major versions match.
///
/// # Example
///
/// ```
/// use awp_types::AwpVersion;
///
/// let v1 = AwpVersion { major: 1, minor: 0 };
/// let v1_1 = AwpVersion { major: 1, minor: 1 };
/// assert!(v1.is_compatible(&v1_1));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AwpVersion {
    pub major: u32,
    pub minor: u32,
}

/// The current AWP protocol version.
pub const CURRENT_VERSION: AwpVersion = AwpVersion { major: 1, minor: 0 };

impl AwpVersion {
    /// Returns `true` if this version is compatible with `other`.
    ///
    /// Compatibility is determined by matching major versions.
    pub fn is_compatible(&self, other: &AwpVersion) -> bool {
        self.major == other.major
    }
}

impl fmt::Display for AwpVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Error returned when parsing an [`AwpVersion`] from a string fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseVersionError(pub String);

impl fmt::Display for ParseVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid AWP version: {}", self.0)
    }
}

impl std::error::Error for ParseVersionError {}

impl FromStr for AwpVersion {
    type Err = ParseVersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 2 {
            return Err(ParseVersionError(format!("expected format 'major.minor', got '{s}'")));
        }
        let major = parts[0]
            .parse::<u32>()
            .map_err(|e| ParseVersionError(format!("invalid major version: {e}")))?;
        let minor = parts[1]
            .parse::<u32>()
            .map_err(|e| ParseVersionError(format!("invalid minor version: {e}")))?;
        Ok(AwpVersion { major, minor })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_version() {
        assert_eq!(CURRENT_VERSION.major, 1);
        assert_eq!(CURRENT_VERSION.minor, 0);
    }

    #[test]
    fn test_display() {
        let v = AwpVersion { major: 1, minor: 0 };
        assert_eq!(v.to_string(), "1.0");

        let v2 = AwpVersion { major: 2, minor: 3 };
        assert_eq!(v2.to_string(), "2.3");
    }

    #[test]
    fn test_from_str_valid() {
        let v: AwpVersion = "1.0".parse().unwrap();
        assert_eq!(v, AwpVersion { major: 1, minor: 0 });

        let v2: AwpVersion = "2.3".parse().unwrap();
        assert_eq!(v2, AwpVersion { major: 2, minor: 3 });
    }

    #[test]
    fn test_from_str_round_trip() {
        let v = AwpVersion { major: 1, minor: 0 };
        let s = v.to_string();
        let parsed: AwpVersion = s.parse().unwrap();
        assert_eq!(v, parsed);
    }

    #[test]
    fn test_from_str_invalid() {
        assert!("abc".parse::<AwpVersion>().is_err());
        assert!("1".parse::<AwpVersion>().is_err());
        assert!("1.2.3".parse::<AwpVersion>().is_err());
        assert!("a.b".parse::<AwpVersion>().is_err());
    }

    #[test]
    fn test_is_compatible_same_major() {
        let a = AwpVersion { major: 1, minor: 0 };
        let b = AwpVersion { major: 1, minor: 5 };
        assert!(a.is_compatible(&b));
    }

    #[test]
    fn test_is_compatible_different_major() {
        let a = AwpVersion { major: 1, minor: 0 };
        let b = AwpVersion { major: 2, minor: 0 };
        assert!(!a.is_compatible(&b));
    }
}
