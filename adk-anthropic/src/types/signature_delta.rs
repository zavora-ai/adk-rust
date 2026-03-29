use serde::{Deserialize, Serialize};

/// A signature delta, representing a piece of a signature in a streaming response.
///
/// SignatureDelta is used in streaming responses to deliver incremental updates
/// to a signature, typically associated with thinking blocks where the model's
/// reasoning process needs to be cryptographically signed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignatureDelta {
    /// The signature content.
    ///
    /// This contains a fragment of the signature that should be appended to
    /// previously received signature fragments to build the complete signature.
    pub signature: String,
}

impl SignatureDelta {
    /// Create a new `SignatureDelta` with the given signature.
    pub fn new(signature: String) -> Self {
        Self { signature }
    }
}

impl std::str::FromStr for SignatureDelta {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::new(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn signature_delta_serialization() {
        let delta = SignatureDelta::new("Robert Paulson".to_string());
        let json = to_value(&delta).unwrap();

        assert_eq!(
            json,
            json!({
                "signature": "Robert Paulson"
            })
        );
    }

    #[test]
    fn signature_delta_deserialization() {
        let json = json!({
            "signature": "Robert Paulson"
        });

        let delta: SignatureDelta = serde_json::from_value(json).unwrap();
        assert_eq!(delta.signature, "Robert Paulson");
    }

    #[test]
    fn from_str() {
        let delta = "Robert Paulson".parse::<SignatureDelta>().unwrap();
        assert_eq!(delta.signature, "Robert Paulson");
    }
}
