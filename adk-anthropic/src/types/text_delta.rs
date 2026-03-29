use serde::{Deserialize, Serialize};

/// A text delta, representing a piece of text in a streaming response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextDelta {
    /// The text content.
    pub text: String,
}

impl TextDelta {
    /// Create a new `TextDelta` with the given text.
    pub fn new(text: String) -> Self {
        Self { text }
    }
}

impl std::str::FromStr for TextDelta {
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
    fn text_delta_serialization() {
        let delta = TextDelta::new("Hello world".to_string());
        let json = to_value(&delta).unwrap();

        assert_eq!(
            json,
            json!({
                "text": "Hello world"
            })
        );
    }

    #[test]
    fn text_delta_deserialization() {
        let json = json!({
            "text": "Hello world"
        });

        let delta: TextDelta = serde_json::from_value(json).unwrap();
        assert_eq!(delta.text, "Hello world");
    }

    #[test]
    fn from_str() {
        let delta = "Hello world".parse::<TextDelta>().unwrap();
        assert_eq!(delta.text, "Hello world");
    }
}
