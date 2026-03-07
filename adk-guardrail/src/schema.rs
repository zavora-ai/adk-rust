use crate::{Guardrail, GuardrailError, GuardrailResult, Severity};
use adk_core::Content;
use async_trait::async_trait;
use jsonschema::Validator;
use serde_json::Value;

/// JSON Schema validator guardrail for enforcing output structure
pub struct SchemaValidator {
    name: String,
    validator: Validator,
    severity: Severity,
}

impl SchemaValidator {
    /// Create a new schema validator from a JSON Schema value
    pub fn new(schema: &Value) -> Result<Self, GuardrailError> {
        let validator = Validator::new(schema)
            .map_err(|e| GuardrailError::Schema(format!("Invalid schema: {}", e)))?;

        Ok(Self { name: "schema_validator".to_string(), validator, severity: Severity::High })
    }

    /// Create with a custom name
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set severity level
    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }

    fn extract_json(&self, content: &Content) -> Option<Value> {
        for part in &content.parts {
            if let Some(text) = part.as_text() {
                // Try to parse as JSON directly
                if let Ok(json) = serde_json::from_str(text) {
                    return Some(json);
                }
                // Try to extract JSON from markdown code block
                if let Some(json_str) = Self::extract_json_from_markdown(text) {
                    if let Ok(json) = serde_json::from_str(&json_str) {
                        return Some(json);
                    }
                }
            }
        }
        None
    }

    fn extract_json_from_markdown(text: &str) -> Option<String> {
        // Look for ```json ... ``` blocks
        let start_markers = ["```json\n", "```json\r\n", "```\n", "```\r\n"];
        let end_marker = "```";

        for start in start_markers {
            if let Some(start_idx) = text.find(start) {
                let content_start = start_idx + start.len();
                if let Some(end_idx) = text[content_start..].find(end_marker) {
                    return Some(text[content_start..content_start + end_idx].trim().to_string());
                }
            }
        }
        None
    }
}

#[async_trait]
impl Guardrail for SchemaValidator {
    fn name(&self) -> &str {
        &self.name
    }

    async fn validate(&self, content: &Content) -> GuardrailResult {
        let json = match self.extract_json(content) {
            Some(j) => j,
            None => {
                return GuardrailResult::Fail {
                    reason: "Content does not contain valid JSON".to_string(),
                    severity: self.severity,
                };
            }
        };

        let result = self.validator.validate(&json);
        if let Err(error) = result {
            return GuardrailResult::Fail {
                reason: format!("Schema validation failed: {}", error),
                severity: self.severity,
            };
        }

        GuardrailResult::Pass
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer", "minimum": 0 }
            },
            "required": ["name"]
        })
    }

    #[tokio::test]
    async fn test_valid_json() {
        let validator = SchemaValidator::new(&test_schema()).unwrap();
        let content = Content::new("model").with_text(r#"{"name": "Alice", "age": 30}"#);
        let result = validator.validate(&content).await;
        assert!(result.is_pass());
    }

    #[tokio::test]
    async fn test_invalid_json_missing_required() {
        let validator = SchemaValidator::new(&test_schema()).unwrap();
        let content = Content::new("model").with_text(r#"{"age": 30}"#);
        let result = validator.validate(&content).await;
        assert!(result.is_fail());
    }

    #[tokio::test]
    async fn test_invalid_json_wrong_type() {
        let validator = SchemaValidator::new(&test_schema()).unwrap();
        let content = Content::new("model").with_text(r#"{"name": "Alice", "age": "thirty"}"#);
        let result = validator.validate(&content).await;
        assert!(result.is_fail());
    }

    #[tokio::test]
    async fn test_json_in_markdown() {
        let validator = SchemaValidator::new(&test_schema()).unwrap();
        let content = Content::new("model")
            .with_text("Here is the result:\n```json\n{\"name\": \"Bob\"}\n```");
        let result = validator.validate(&content).await;
        assert!(result.is_pass());
    }

    #[tokio::test]
    async fn test_no_json() {
        let validator = SchemaValidator::new(&test_schema()).unwrap();
        let content = Content::new("model").with_text("This is just plain text");
        let result = validator.validate(&content).await;
        assert!(result.is_fail());
    }
}
