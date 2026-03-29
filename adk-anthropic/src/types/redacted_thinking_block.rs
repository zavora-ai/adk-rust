use serde::{Deserialize, Serialize};

/// A redacted thinking block that contains encoded/obscured thinking data.
///
/// This block is used when the full thinking contents are not directly accessible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RedactedThinkingBlock {
    /// The encoded thinking data (redacted from normal display).
    pub data: String,
}

impl RedactedThinkingBlock {
    /// Creates a new RedactedThinkingBlock with the specified data.
    pub fn new<S: Into<String>>(data: S) -> Self {
        Self { data: data.into() }
    }
}

impl std::str::FromStr for RedactedThinkingBlock {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::new(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn redacted_thinking_block_serialization() {
        let block = RedactedThinkingBlock::new("encoded-thinking-data-123");

        let json = serde_json::to_string(&block).unwrap();
        let expected = r#"{"data":"encoded-thinking-data-123"}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn deserialization() {
        let json = r#"{"data":"encoded-thinking-data-123","type":"redacted_thinking"}"#;
        let block: RedactedThinkingBlock = serde_json::from_str(json).unwrap();

        assert_eq!(block.data, "encoded-thinking-data-123");
    }

    #[test]
    fn new_with_str() {
        let block = RedactedThinkingBlock::new("Redacted thinking content");
        let json = to_value(&block).unwrap();

        assert_eq!(
            json,
            json!({
                "data": "Redacted thinking content"
            })
        );
    }

    #[test]
    fn from_str() {
        let block = "Redacted thinking content".parse::<RedactedThinkingBlock>().unwrap();
        assert_eq!(block.data, "Redacted thinking content");
    }
}
