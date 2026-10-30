use std::ops::Range;

use crate::{Guardrail, GuardrailResult};
use adk_core::{Content, Part};
use async_trait::async_trait;
use regex::Regex;

/// Digit-group layouts a card number is written in besides one contiguous run: 4-4-4-4,
/// 19 digits as 4-4-4-4-3, and American Express 4-6-5. Longest first, so a card followed by a
/// short number, such as a security code, still matches its own layout.
const CARD_GROUPINGS: [&[usize]; 3] = [&[4, 4, 4, 4, 3], &[4, 4, 4, 4], &[4, 6, 5]];

/// Types of PII to detect and redact
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PiiType {
    /// An email address whose top-level domain has two or more letters.
    Email,
    /// A North American phone number, with an optional `+1` prefix.
    Phone,
    /// A US Social Security number, written `123-45-6789` or as nine digits.
    Ssn,
    /// A payment card number of 13 to 19 digits that passes the Luhn check, written as one
    /// run of digits or grouped 4-4-4-4, 4-6-5 (American Express), or 4-4-4-4-3, with a single
    /// space or hyphen between groups.
    CreditCard,
    /// A dotted-quad IPv4 address. Not among [`PiiRedactor::new`]'s types.
    IpAddress,
}

impl PiiType {
    fn pattern(&self) -> &'static str {
        match self {
            PiiType::Email => r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b",
            PiiType::Phone => r"\b(?:\+?1[-.\s]?)?\(?[0-9]{3}\)?[-.\s]?[0-9]{3}[-.\s]?[0-9]{4}\b",
            PiiType::Ssn => r"\b\d{3}[-\s]?\d{2}[-\s]?\d{4}\b",
            // A run of digit groups joined by single separators; `card_spans` finds the card
            // numbers inside it.
            PiiType::CreditCard => r"\b[0-9]+(?:[-\s][0-9]+)*\b",
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

/// Byte ranges of the card numbers in `text`.
///
/// Each run of digit groups found by `runs` is scanned from its first group: a single group of
/// 13 to 19 digits, or consecutive groups in one of the [`CARD_GROUPINGS`], is a card number
/// when its digits pass the Luhn check. A run is never split inside a group.
fn card_spans(runs: &Regex, text: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    for run in runs.find_iter(text) {
        // Separators may be multi-byte whitespace, but no byte of one is an ASCII digit.
        let mut groups: Vec<Range<usize>> = Vec::new();
        let mut group_start = None;
        for (offset, byte) in run.as_str().bytes().enumerate() {
            if byte.is_ascii_digit() {
                group_start.get_or_insert(run.start() + offset);
            } else if let Some(start) = group_start.take() {
                groups.push(start..run.start() + offset);
            }
        }
        if let Some(start) = group_start {
            groups.push(start..run.end());
        }

        let mut first = 0;
        while first < groups.len() {
            let rest = &groups[first..];
            let contiguous = (13..=19).contains(&rest[0].len()).then_some(1);
            let grouped = CARD_GROUPINGS.iter().filter_map(|layout| {
                let fits = rest.len() >= layout.len()
                    && rest.iter().zip(layout.iter()).all(|(group, &len)| group.len() == len);
                fits.then_some(layout.len())
            });
            let card = contiguous.into_iter().chain(grouped).find(|&count| {
                // Luhn: from the rightmost digit, double every second digit and sum the digits.
                let digits = rest[..count].iter().flat_map(|group| {
                    text[group.clone()].bytes().map(|byte| u32::from(byte - b'0'))
                });
                let sum: u32 = digits
                    .rev()
                    .enumerate()
                    .map(|(position, digit)| match (position % 2, digit * 2) {
                        (1, doubled) if doubled > 9 => doubled - 9,
                        (1, doubled) => doubled,
                        _ => digit,
                    })
                    .sum();
                sum.is_multiple_of(10)
            });
            match card {
                Some(count) => {
                    spans.push(rest[0].start..rest[count - 1].end);
                    first += count;
                }
                None => first += 1,
            }
        }
    }
    spans
}

/// PII detection and redaction guardrail
pub struct PiiRedactor {
    patterns: Vec<(PiiType, Regex)>,
}

impl PiiRedactor {
    /// Creates a redactor for email addresses, phone numbers, SSNs, and card numbers.
    ///
    /// IP addresses are not redacted by default. To redact them as well, name every type with
    /// [`PiiRedactor::with_types`], including [`PiiType::IpAddress`].
    ///
    /// # Example
    ///
    /// ```
    /// use adk_guardrail::{PiiRedactor, PiiType};
    ///
    /// let (text, found) = PiiRedactor::new().redact("Card 3782 822463 10005 from 192.168.1.1");
    /// assert_eq!(text, "Card [CREDIT CARD REDACTED] from 192.168.1.1");
    /// assert_eq!(found, vec![PiiType::CreditCard]);
    ///
    /// let with_ip = PiiRedactor::with_types(&[
    ///     PiiType::Email,
    ///     PiiType::Phone,
    ///     PiiType::Ssn,
    ///     PiiType::CreditCard,
    ///     PiiType::IpAddress,
    /// ]);
    /// let (text, _) = with_ip.redact("Card 3782 822463 10005 from 192.168.1.1");
    /// assert_eq!(text, "Card [CREDIT CARD REDACTED] from [IP REDACTED]");
    /// ```
    pub fn new() -> Self {
        Self::with_types(&[PiiType::Email, PiiType::Phone, PiiType::Ssn, PiiType::CreditCard])
    }

    /// Creates a redactor for the given types, applied in the order given.
    pub fn with_types(types: &[PiiType]) -> Self {
        let patterns =
            types.iter().filter_map(|t| Regex::new(t.pattern()).ok().map(|r| (*t, r))).collect();

        Self { patterns }
    }

    /// Redacts PII from text, returning the redacted text and the types found.
    pub fn redact(&self, text: &str) -> (String, Vec<PiiType>) {
        let mut result = text.to_string();
        let mut found = Vec::new();

        for (pii_type, regex) in &self.patterns {
            let spans = match pii_type {
                PiiType::CreditCard => card_spans(regex, &result),
                PiiType::Email | PiiType::Phone | PiiType::Ssn | PiiType::IpAddress => {
                    regex.find_iter(&result).map(|m| m.range()).collect()
                }
            };
            if spans.is_empty() {
                continue;
            }
            found.push(*pii_type);
            let mut redacted = String::with_capacity(result.len());
            let mut kept_from = 0;
            for span in spans {
                redacted.push_str(&result[kept_from..span.start]);
                redacted.push_str(pii_type.redaction());
                kept_from = span.end;
            }
            redacted.push_str(&result[kept_from..]);
            result = redacted;
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
                Part::Text { text } => {
                    let (redacted, found) = self.redact(text);
                    if !found.is_empty() {
                        any_redacted = true;
                        redacted_types.extend(found);
                        new_parts.push(Part::Text { text: redacted });
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
    fn amex_card_numbers_are_redacted() {
        let redactor = PiiRedactor::new();
        let (result, found) = redactor.redact("Amex 3782 822463 10005, or 371449635398431.");
        assert_eq!(
            (result.as_str(), found),
            ("Amex [CREDIT CARD REDACTED], or [CREDIT CARD REDACTED].", vec![PiiType::CreditCard])
        );
    }

    #[test]
    fn nineteen_digit_card_numbers_are_redacted_whole() {
        let redactor = PiiRedactor::new();
        let (result, found) = redactor.redact(
            "UnionPay 6205500000000000004 or 6205 5000 0000 0000 004 or 6205-5000-0000-0000-004",
        );
        assert_eq!(
            (result.as_str(), found),
            (
                "UnionPay [CREDIT CARD REDACTED] or [CREDIT CARD REDACTED] or [CREDIT CARD REDACTED]",
                vec![PiiType::CreditCard]
            )
        );
    }

    #[test]
    fn thirteen_digit_card_numbers_are_redacted() {
        let redactor = PiiRedactor::new();
        let (result, _) = redactor.redact("Visa 4222222222222");
        assert_eq!(result, "Visa [CREDIT CARD REDACTED]");
    }

    #[test]
    fn only_the_luhn_check_digit_completes_a_card_number() {
        let redactor = PiiRedactor::new();
        let check_digits = |prefix: &str| -> Vec<char> {
            ('0'..='9')
                .filter(|digit| !redactor.redact(&format!("{prefix}{digit}")).1.is_empty())
                .collect()
        };
        assert_eq!(
            ["37828224631000", "411111111111111", "620550000000000000"].map(check_digits),
            [vec!['5'], vec!['1'], vec!['4']]
        );
    }

    #[test]
    fn numbers_failing_the_luhn_check_are_kept() {
        let redactor = PiiRedactor::new();
        let text = "Order 2026 1028 0042 7315, tracking 1234567890123456, ref 4111-1111-1111-1112";
        let (result, found) = redactor.redact(text);
        assert_eq!((result.as_str(), found), (text, vec![]));
    }

    #[test]
    fn a_card_followed_by_a_short_number_is_redacted_on_its_own() {
        let redactor = PiiRedactor::new();
        let (result, _) =
            redactor.redact("4111 1111 1111 1111 737, expires 4111 1111 1111 1111 12/27");
        assert_eq!(result, "[CREDIT CARD REDACTED] 737, expires [CREDIT CARD REDACTED] 12/27");
    }

    #[test]
    fn a_card_inside_a_longer_run_of_groups_is_redacted() {
        let redactor = PiiRedactor::new();
        let (result, _) = redactor.redact("Ref 12 4111 1111 1111 1111 9876 x");
        assert_eq!(result, "Ref 12 [CREDIT CARD REDACTED] 9876 x");
    }

    #[test]
    fn email_redaction_stops_at_a_pipe() {
        let redactor = PiiRedactor::new();
        let (result, found) = redactor.redact("jane.doe@example.com|ticket-7 and a@b.com|");
        assert_eq!(
            (result.as_str(), found),
            ("[EMAIL REDACTED]|ticket-7 and [EMAIL REDACTED]|", vec![PiiType::Email])
        );
    }

    #[test]
    fn ip_addresses_are_redacted_only_when_named() {
        let text = "Host 192.168.1.1";
        assert_eq!(PiiRedactor::new().redact(text), (text.to_string(), vec![]));

        let with_ip = PiiRedactor::with_types(&[
            PiiType::Email,
            PiiType::Phone,
            PiiType::Ssn,
            PiiType::CreditCard,
            PiiType::IpAddress,
        ]);
        assert_eq!(
            with_ip.redact(text),
            ("Host [IP REDACTED]".to_string(), vec![PiiType::IpAddress])
        );
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
                let text = new_content.parts[0].text().unwrap();
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
