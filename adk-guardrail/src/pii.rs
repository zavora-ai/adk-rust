use crate::{Guardrail, GuardrailResult};
use adk_core::{Content, Part};
use async_trait::async_trait;
use regex::Regex;

/// Types of PII to detect and redact
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PiiType {
    Email,
    Phone,
    Ssn,
    CreditCard,
    IpAddress,
}

impl PiiType {
    fn pattern(&self) -> &'static str {
        match self {
            PiiType::Email => r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Z|a-z]{2,}\b",
            PiiType::Phone => r"\b(?:\+?1[-.\s]?)?\(?[0-9]{3}\)?[-.\s]?[0-9]{3}[-.\s]?[0-9]{4}\b",
            PiiType::Ssn => r"\b\d{3}[-\s]?\d{2}[-\s]?\d{4}\b",
            PiiType::CreditCard => r"\b(?:\d{4}[-\s]?){3}\d{4}\b",
            PiiType::IpAddress => r"\b(?:\d{1,3}\.){3}\d{1,3}\b",
        }
    }

    fn redaction(&self) -> &'static str {
        match self {
            PiiType::Email => "[EMAIL REDACTED]",
            PiiType::Phone => "[PHONE REDACTED]",
            PiiType::Ssn => "[SSN REDACTED]",
            PiiType::CreditCard => "[CREDIT CARD REDACTED]",
            PiiType::IpAddress => "[IP REDACTED]",
        }
    }
}

/// PII detection and redaction guardrail
pub struct PiiRedactor {
    patterns: Vec<(PiiType, Regex)>,
}

impl PiiRedactor {
    /// Create a new PII redactor with all PII types enabled
    pub fn new() -> Self {
        Self::with_types(&[PiiType::Email, PiiType::Phone, PiiType::Ssn, PiiType::CreditCard])
    }

    /// Create a PII redactor with specific types
    pub fn with_types(types: &[PiiType]) -> Self {
        let patterns =
            types.iter().filter_map(|t| Regex::new(t.pattern()).ok().map(|r| (*t, r))).collect();

        Self { patterns }
    }

    /// Redact PII from text, returns (redacted_text, found_types)
    pub fn redact(&self, text: &str) -> (String, Vec<PiiType>) {
        let mut result = text.to_string();
        let mut found = Vec::new();

        for (pii_type, regex) in &self.patterns {
            if regex.is_match(&result) {
                found.push(*pii_type);
                result = regex.replace_all(&result, pii_type.redaction()).to_string();
            }
        }

        (result, found)
    }
}

impl Default for PiiRedactor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Guardrail for PiiRedactor {
    fn name(&self) -> &str {
        "pii_redactor"
    }

    async fn validate(&self, content: &Content) -> GuardrailResult {
        let mut new_parts = Vec::new();
        let mut any_redacted = false;
        let mut redacted_types = Vec::new();

        for part in &content.parts {
            match part {
                Part::Text(text) => {
                    let (redacted, found) = self.redact(text);
                    if !found.is_empty() {
                        any_redacted = true;
                        redacted_types.extend(found);
                        new_parts.push(Part::text(redacted));
                    } else {
                        new_parts.push(part.clone());
                    }
                }
                _ => new_parts.push(part.clone()),
            }
        }

        if any_redacted {
            let types_str: Vec<_> = redacted_types.iter().map(|t| format!("{:?}", t)).collect();
            GuardrailResult::Transform {
                new_content: Content { role: content.role.clone(), parts: new_parts },
                reason: format!("Redacted PII types: {}", types_str.join(", ")),
            }
        } else {
            GuardrailResult::Pass
        }
    }

    fn run_parallel(&self) -> bool {
        false // Must run sequentially to transform content
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_email_redaction() {
        let redactor = PiiRedactor::new();
        let (result, found) = redactor.redact("Contact me at test@example.com");
        assert_eq!(result, "Contact me at [EMAIL REDACTED]");
        assert!(found.contains(&PiiType::Email));
    }

    #[test]
    fn test_phone_redaction() {
        let redactor = PiiRedactor::new();
        let (result, found) = redactor.redact("Call me at 555-123-4567");
        assert_eq!(result, "Call me at [PHONE REDACTED]");
        assert!(found.contains(&PiiType::Phone));
    }

    #[test]
    fn test_ssn_redaction() {
        let redactor = PiiRedactor::new();
        let (result, found) = redactor.redact("SSN: 123-45-6789");
        assert_eq!(result, "SSN: [SSN REDACTED]");
        assert!(found.contains(&PiiType::Ssn));
    }

    #[test]
    fn test_credit_card_redaction() {
        let redactor = PiiRedactor::new();
        let (result, found) = redactor.redact("Card: 4111-1111-1111-1111");
        assert_eq!(result, "Card: [CREDIT CARD REDACTED]");
        assert!(found.contains(&PiiType::CreditCard));
    }

    #[test]
    fn test_multiple_pii() {
        let redactor = PiiRedactor::new();
        let (result, found) = redactor.redact("Email: a@b.com, Phone: 555-123-4567");
        assert!(result.contains("[EMAIL REDACTED]"));
        assert!(result.contains("[PHONE REDACTED]"));
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn test_no_pii() {
        let redactor = PiiRedactor::new();
        let (result, found) = redactor.redact("Hello world");
        assert_eq!(result, "Hello world");
        assert!(found.is_empty());
    }

    #[tokio::test]
    async fn test_guardrail_transform() {
        let redactor = PiiRedactor::new();
        let content = Content::new("user").with_text("Email: test@example.com");
        let result = redactor.validate(&content).await;

        match result {
            GuardrailResult::Transform { new_content, .. } => {
                let text = new_content.parts[0].as_text().unwrap();
                assert!(text.contains("[EMAIL REDACTED]"));
            }
            _ => panic!("Expected Transform result"),
        }
    }

    #[tokio::test]
    async fn test_guardrail_pass() {
        let redactor = PiiRedactor::new();
        let content = Content::new("user").with_text("Hello world");
        let result = redactor.validate(&content).await;
        assert!(result.is_pass());
    }
}
