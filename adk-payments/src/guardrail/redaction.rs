use std::collections::HashMap;

use adk_core::{Content, Part};
use adk_guardrail::{Guardrail, GuardrailResult, PiiRedactor, PiiType};
use async_trait::async_trait;
use regex::{Captures, Regex};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Content redactor for card data, billing PII, and signed payment artifacts.
pub struct SensitivePaymentDataGuardrail {
    pii_redactor: PiiRedactor,
    card_number_regex: Regex,
    cvc_regex: Regex,
    expiry_regex: Regex,
    billing_address_regex: Regex,
    keyed_secret_regex: Regex,
}

impl SensitivePaymentDataGuardrail {
    /// Creates a new payment-data redactor.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Redacts sensitive payment material from plain text.
    #[must_use]
    pub fn redact_text(&self, text: &str) -> String {
        let keyed_secrets = self
            .keyed_secret_regex
            .replace_all(text, |captures: &Captures<'_>| {
                let key = captures.name("key").map_or("secret", |value| value.as_str());
                let value = captures.name("value").map_or("", |value| value.as_str());
                format!("{key}: {}", digest_marker(value))
            })
            .to_string();
        let cvc_redacted = self
            .cvc_regex
            .replace_all(&keyed_secrets, |captures: &Captures<'_>| {
                let key = captures.name("key").map_or("cvc", |value| value.as_str());
                format!("{key}: [CVC REDACTED]")
            })
            .to_string();
        let expiry_redacted = self
            .expiry_regex
            .replace_all(&cvc_redacted, |captures: &Captures<'_>| {
                let key = captures.name("key").map_or("expiry", |value| value.as_str());
                format!("{key}: [EXPIRY REDACTED]")
            })
            .to_string();
        let billing_redacted = self
            .billing_address_regex
            .replace_all(&expiry_redacted, "billing address: [BILLING DETAILS REDACTED]")
            .to_string();
        let card_masked = self
            .card_number_regex
            .replace_all(&billing_redacted, |captures: &Captures<'_>| {
                captures.get(0).map_or_else(
                    || "[CARD REDACTED]".to_string(),
                    |value| mask_card_number(value.as_str()),
                )
            })
            .to_string();
        let (pii_redacted, _) = self.pii_redactor.redact(&card_masked);
        pii_redacted
    }

    /// Redacts sensitive payment material from ADK content parts.
    #[must_use]
    pub fn redact_content(&self, content: &Content) -> Content {
        self.redact_content_internal(content).0
    }

    /// Redacts sensitive payment material from JSON payloads.
    #[must_use]
    pub fn redact_json(&self, value: &Value) -> Value {
        self.redact_json_internal(None, value)
    }

    /// Redacts sensitive payment material from telemetry span fields.
    #[must_use]
    pub fn redact_telemetry_fields(
        &self,
        fields: &HashMap<String, String>,
    ) -> HashMap<String, String> {
        fields
            .iter()
            .map(|(key, value)| {
                let redacted =
                    self.redact_json_internal(Some(key.as_str()), &Value::String(value.clone()));
                let value = match redacted {
                    Value::String(value) => value,
                    other => other.to_string(),
                };
                (key.clone(), value)
            })
            .collect()
    }

    fn redact_content_internal(&self, content: &Content) -> (Content, bool) {
        let mut changed = false;
        let mut new_parts = Vec::with_capacity(content.parts.len());

        for part in &content.parts {
            match part {
                Part::Text { text } => {
                    let redacted = self.redact_text(text);
                    if redacted != *text {
                        changed = true;
                    }
                    new_parts.push(Part::Text { text: redacted });
                }
                Part::Thinking { thinking, signature } => {
                    let redacted = self.redact_text(thinking);
                    if redacted != *thinking {
                        changed = true;
                    }
                    new_parts
                        .push(Part::Thinking { thinking: redacted, signature: signature.clone() });
                }
                Part::FunctionCall { name, args, id, thought_signature } => {
                    let redacted_args = self.redact_json(args);
                    if redacted_args != *args {
                        changed = true;
                    }
                    new_parts.push(Part::FunctionCall {
                        name: name.clone(),
                        args: redacted_args,
                        id: id.clone(),
                        thought_signature: thought_signature.clone(),
                    });
                }
                Part::FunctionResponse { function_response, id } => {
                    let redacted_response = self.redact_json(&function_response.response);
                    if redacted_response != function_response.response {
                        changed = true;
                    }
                    new_parts.push(Part::FunctionResponse {
                        function_response: adk_core::FunctionResponseData {
                            name: function_response.name.clone(),
                            response: redacted_response,
                        },
                        id: id.clone(),
                    });
                }
                _ => new_parts.push(part.clone()),
            }
        }

        (Content { role: content.role.clone(), parts: new_parts }, changed)
    }

    fn redact_json_internal(&self, key: Option<&str>, value: &Value) -> Value {
        if let Some(key) = key {
            let normalized = normalize_key(key);
            if is_card_key(&normalized) {
                return redact_card_value(value);
            }
            if is_cvc_key(&normalized) {
                return Value::String("[CVC REDACTED]".to_string());
            }
            if is_expiry_key(&normalized) {
                return Value::String("[EXPIRY REDACTED]".to_string());
            }
            if is_secret_key(&normalized) {
                return Value::String(digest_marker(&canonical_value(value)));
            }
            if is_email_key(&normalized) || is_phone_key(&normalized) {
                return match value {
                    Value::String(text) => Value::String(self.redact_text(text)),
                    _ => Value::String("[PII REDACTED]".to_string()),
                };
            }
            if is_billing_key(&normalized) {
                return minimize_billing_value(value);
            }
        }

        match value {
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .map(|(child_key, child_value)| {
                        (child_key.clone(), self.redact_json_internal(Some(child_key), child_value))
                    })
                    .collect::<Map<String, Value>>(),
            ),
            Value::Array(values) => Value::Array(
                values.iter().map(|child| self.redact_json_internal(None, child)).collect(),
            ),
            Value::String(text) => Value::String(self.redact_text(text)),
            _ => value.clone(),
        }
    }
}

impl Default for SensitivePaymentDataGuardrail {
    fn default() -> Self {
        Self {
            pii_redactor: PiiRedactor::with_types(&[PiiType::Email, PiiType::Phone, PiiType::Ssn]),
            card_number_regex: Regex::new(r"\b(?:\d[ -]?){13,19}\b").unwrap(),
            cvc_regex: Regex::new(
                r"(?i)\b(?P<key>cvv|cvc|cid|security[_ -]?code)\b\s*[:=]\s*(?P<value>\d{3,4})",
            )
            .unwrap(),
            expiry_regex: Regex::new(
                r"(?i)\b(?P<key>exp(?:iry|iration)?(?:[_ -]?date)?)\b\s*[:=]\s*(?P<value>(?:0[1-9]|1[0-2])[/-]\d{2,4})",
            )
            .unwrap(),
            billing_address_regex: Regex::new(
                r"(?i)\bbilling(?:[_ ]address)?\b\s*[:=]\s*(?P<value>[^\n;]+)",
            )
            .unwrap(),
            keyed_secret_regex: Regex::new(
                r"(?i)\b(?P<key>signed_?authorization|authorization(?:_blob)?|merchant_?signature|buyer_?signature|signature|signed_?mandate|cryptogram|payment_?token|delegated_?credential|continuation_?token|nonce|jwt|jws)\b\s*[:=]\s*(?P<value>[A-Za-z0-9._:+/=-]{8,})",
            )
            .unwrap(),
        }
    }
}

#[async_trait]
impl Guardrail for SensitivePaymentDataGuardrail {
    fn name(&self) -> &str {
        "sensitive_payment_data"
    }

    async fn validate(&self, content: &Content) -> GuardrailResult {
        let (new_content, changed) = self.redact_content_internal(content);
        if changed {
            GuardrailResult::transform(
                new_content,
                "redacted payment card, billing, or signed authorization material",
            )
        } else {
            GuardrailResult::pass()
        }
    }

    fn run_parallel(&self) -> bool {
        false
    }
}

/// Redacts sensitive payment material from plain text.
#[must_use]
pub fn redact_payment_text(text: &str) -> String {
    SensitivePaymentDataGuardrail::new().redact_text(text)
}

/// Redacts sensitive payment material from ADK content.
#[must_use]
pub fn redact_payment_content(content: &Content) -> Content {
    SensitivePaymentDataGuardrail::new().redact_content(content)
}

/// Redacts sensitive payment material from JSON payloads.
#[must_use]
pub fn redact_payment_value(value: &Value) -> Value {
    SensitivePaymentDataGuardrail::new().redact_json(value)
}

/// Redacts sensitive payment material from tool JSON outputs.
#[must_use]
pub fn redact_tool_output(value: &Value) -> Value {
    redact_payment_value(value)
}

/// Redacts sensitive payment material from telemetry span fields.
#[must_use]
pub fn redact_telemetry_fields(fields: &HashMap<String, String>) -> HashMap<String, String> {
    SensitivePaymentDataGuardrail::new().redact_telemetry_fields(fields)
}

fn redact_card_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(mask_card_number(text)),
        _ => Value::String("[CARD REDACTED]".to_string()),
    }
}

fn minimize_billing_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut minimized = Map::new();

            if let Some(country) = object
                .get("country")
                .or_else(|| object.get("countryCode"))
                .or_else(|| object.get("country_code"))
                .and_then(Value::as_str)
            {
                minimized.insert("country".to_string(), Value::String(country.to_string()));
            }

            if let Some(postal_code) = object
                .get("postalCode")
                .or_else(|| object.get("postal_code"))
                .or_else(|| object.get("zip"))
                .or_else(|| object.get("zipCode"))
                .and_then(Value::as_str)
            {
                minimized.insert(
                    "postalCodeMasked".to_string(),
                    Value::String(mask_postal_code(postal_code)),
                );
            }

            if minimized.is_empty() {
                Value::String("[BILLING DETAILS REDACTED]".to_string())
            } else {
                Value::Object(minimized)
            }
        }
        _ => Value::String("[BILLING DETAILS REDACTED]".to_string()),
    }
}

fn is_card_key(key: &str) -> bool {
    key == "pan"
        || key.contains("cardnumber")
        || key.contains("primaryaccountnumber")
        || key.contains("paymentcard")
}

fn is_cvc_key(key: &str) -> bool {
    key == "cvv" || key == "cvc" || key == "cid" || key.contains("securitycode")
}

fn is_expiry_key(key: &str) -> bool {
    key == "exp" || key.contains("expiry") || key.contains("expiration") || key.contains("expdate")
}

fn is_secret_key(key: &str) -> bool {
    key.contains("signature")
        || key.contains("signedauthorization")
        || key.contains("authorizationblob")
        || key.contains("signedmandate")
        || key.contains("cryptogram")
        || key.contains("token")
        || key.contains("nonce")
        || key.contains("delegatedcredential")
        || key == "jwt"
        || key == "jws"
}

fn is_email_key(key: &str) -> bool {
    key == "email" || key.ends_with("email")
}

fn is_phone_key(key: &str) -> bool {
    key == "phone" || key.ends_with("phone") || key.ends_with("phonenumber")
}

fn is_billing_key(key: &str) -> bool {
    key == "billing"
        || key == "billingdetails"
        || key.ends_with("billingaddress")
        || key.starts_with("billingaddress")
}

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|char| char.is_ascii_alphanumeric())
        .map(|char| char.to_ascii_lowercase())
        .collect()
}

fn canonical_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string()),
    }
}

fn digest_marker(value: &str) -> String {
    format!("[REDACTED sha256:{}]", &hex::encode(Sha256::digest(value.as_bytes()))[..16])
}

fn mask_card_number(text: &str) -> String {
    let digits: String = text.chars().filter(|char| char.is_ascii_digit()).collect();
    if digits.len() < 4 {
        "[CARD REDACTED]".to_string()
    } else {
        let last4 = &digits[digits.len() - 4..];
        format!("[CARD ****{last4}]")
    }
}

fn mask_postal_code(postal_code: &str) -> String {
    let mut chars = postal_code.chars();
    let prefix: String = chars.by_ref().take(2).collect();
    if prefix.is_empty() { "***".to_string() } else { format!("{prefix}***") }
}

#[cfg(test)]
mod tests {
    use adk_core::FunctionResponseData;
    use serde_json::json;

    use super::*;

    #[test]
    fn redacts_text_card_signature_and_pii() {
        let redactor = SensitivePaymentDataGuardrail::new();
        let redacted = redactor.redact_text(
            "card 4111-1111-1111-1111 billing address: 123 Main St; email payer@example.com signature=signed_blob",
        );

        assert!(!redacted.contains("4111-1111-1111-1111"));
        assert!(!redacted.contains("payer@example.com"));
        assert!(!redacted.contains("signed_blob"));
        assert!(redacted.contains("[CARD ****1111]"));
        assert!(redacted.contains("[EMAIL REDACTED]"));
        assert!(redacted.contains("[REDACTED sha256:"));
    }

    #[test]
    fn redacts_tool_output_and_minimizes_billing_details() {
        let redacted = redact_tool_output(&json!({
            "cardNumber": "4111111111111111",
            "cvv": "123",
            "billingAddress": {
                "line1": "123 Main St",
                "city": "San Francisco",
                "country": "US",
                "postalCode": "94105"
            },
            "signedAuthorization": "signed_blob",
            "receiptEmail": "payer@example.com"
        }));

        assert_eq!(redacted["cardNumber"], "[CARD ****1111]");
        assert_eq!(redacted["cvv"], "[CVC REDACTED]");
        assert_eq!(redacted["billingAddress"]["country"], "US");
        assert_eq!(redacted["billingAddress"]["postalCodeMasked"], "94***");
        assert!(redacted["signedAuthorization"].as_str().unwrap().starts_with("[REDACTED sha256:"));
        assert_eq!(redacted["receiptEmail"], "[EMAIL REDACTED]");
    }

    #[test]
    fn redacts_telemetry_fields() {
        let mut fields = HashMap::new();
        fields.insert("payment.pan".to_string(), "4111111111111111".to_string());
        fields.insert("payment.signature".to_string(), "signed_blob".to_string());
        fields.insert("billing.email".to_string(), "payer@example.com".to_string());

        let redacted = redact_telemetry_fields(&fields);

        assert_eq!(redacted["payment.pan"], "[CARD ****1111]");
        assert!(redacted["payment.signature"].starts_with("[REDACTED sha256:"));
        assert_eq!(redacted["billing.email"], "[EMAIL REDACTED]");
    }

    #[tokio::test]
    async fn guardrail_transforms_text_and_function_responses() {
        let guardrail = SensitivePaymentDataGuardrail::new();
        let content = Content {
            role: "tool".to_string(),
            parts: vec![
                Part::Text { text: "card 4111 1111 1111 1111".to_string() },
                Part::FunctionResponse {
                    function_response: FunctionResponseData {
                        name: "checkout".to_string(),
                        response: json!({
                            "signedAuthorization": "signed_blob",
                            "billingAddress": {
                                "country": "US",
                                "postalCode": "10001",
                                "line1": "123 Main St"
                            }
                        }),
                    },
                    id: None,
                },
            ],
        };

        match guardrail.validate(&content).await {
            GuardrailResult::Transform { new_content, .. } => {
                let text = new_content.parts[0].text().unwrap();
                assert!(text.contains("[CARD ****1111]"));

                let Part::FunctionResponse { function_response, .. } = &new_content.parts[1] else {
                    panic!("expected function response");
                };
                assert!(
                    function_response.response["signedAuthorization"]
                        .as_str()
                        .unwrap()
                        .starts_with("[REDACTED sha256:")
                );
                assert_eq!(
                    function_response.response["billingAddress"]["postalCodeMasked"],
                    "10***"
                );
            }
            other => panic!("expected transform result, got {other:?}"),
        }
    }
}
