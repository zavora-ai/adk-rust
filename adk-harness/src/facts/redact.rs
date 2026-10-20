//! Redaction of fact text before storage and indexing (spec 3.1 §7.15.2, §9.2 I43).
//!
//! Personal data is replaced with `adk_guardrail::PiiRedactor`'s markers; a secret —
//! a key, a token, a private key, a connection string with a password — rejects the
//! candidate, so no secret is ever stored.

use std::sync::OnceLock;

use adk_guardrail::PiiRedactor;
use regex::Regex;

/// The harness's secret patterns, by name.
const SECRET_PATTERNS: &[(&str, &str)] = &[
    ("private_key", r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----"),
    ("aws_access_key", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
    ("github_token", r"\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{22,})"),
    ("api_key", r"\bsk-(?:[A-Za-z0-9]+-)?[A-Za-z0-9_\-]{20,}"),
    ("stripe_key", r"\b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{16,}"),
    ("google_api_key", r"\bAIza[0-9A-Za-z_\-]{35}\b"),
    ("slack_token", r"\bxox[abprs]-[A-Za-z0-9-]{10,}"),
    ("jwt", r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}"),
    ("bearer_token", r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{20,}"),
    ("connection_string", r"(?i)\b[a-z][a-z0-9+.-]*://[^\s:/@]+:[^\s@/]+@"),
    (
        "secret_assignment",
        r#"(?i)\b(?:api[_-]?key|secret[_-]?key|client[_-]?secret|secret|password|passwd|token|access[_-]?key)\s*[:=]\s*['"]?[A-Za-z0-9_\-./+=]{8,}"#,
    ),
];

fn secret_patterns() -> &'static [(&'static str, Regex)] {
    static PATTERNS: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        SECRET_PATTERNS
            .iter()
            .filter_map(|(name, pattern)| Regex::new(pattern).ok().map(|regex| (*name, regex)))
            .collect()
    })
}

fn pii() -> &'static PiiRedactor {
    static PII: OnceLock<PiiRedactor> = OnceLock::new();
    PII.get_or_init(PiiRedactor::new)
}

/// The name of the first secret pattern `text` matches.
pub(crate) fn find_secret(text: &str) -> Option<&'static str> {
    secret_patterns().iter().find(|(_, regex)| regex.is_match(text)).map(|(name, _)| *name)
}

/// `text` prepared for storage as a fact, as every write path prepares it (spec 3.1
/// §9.2 I43): trimmed, refused when empty, longer than `max_chars` or holding a secret,
/// with personal data redacted.
///
/// # Errors
///
/// Returns `harness.memory.rejected` naming the reason.
///
/// # Example
///
/// ```rust
/// use adk_harness::redact_fact_text;
///
/// assert_eq!(redact_fact_text(" mail ops@example.com ", 500).unwrap(), "mail [EMAIL REDACTED]");
/// assert!(redact_fact_text("password: swordfish123", 500).is_err());
/// ```
pub fn redact_fact_text(text: &str, max_chars: usize) -> adk_core::Result<String> {
    let reject = |reason: String| {
        adk_core::AdkError::new(
            adk_core::ErrorComponent::Tool,
            adk_core::ErrorCategory::InvalidInput,
            "harness.memory.rejected",
            format!("the fact was not recorded: {reason}"),
        )
    };
    let text = text.trim();
    if text.is_empty() {
        return Err(reject("it is empty".into()));
    }
    if text.chars().count() > max_chars {
        return Err(reject(format!("it is longer than {max_chars} characters")));
    }
    if let Some(secret) = find_secret(text) {
        return Err(reject(format!("it holds a secret ({secret})")));
    }
    Ok(redact_pii(text).0)
}

/// `text` with personal data replaced, and whether anything was.
pub(crate) fn redact_pii(text: &str) -> (String, bool) {
    let (redacted, found) = pii().redact(text);
    (redacted, !found.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_found_by_kind() {
        for (text, kind) in [
            ("the key is -----BEGIN OPENSSH PRIVATE KEY----- abc", "private_key"),
            ("use AKIAABCDEFGHIJKLMNOP for s3", "aws_access_key"),
            ("token ghp_abcdefghijklmnopqrstuvwxyz0123456789", "github_token"),
            ("OPENAI key sk-proj-abcdefghijklmnopqrstuvwxyz123456", "api_key"),
            ("connect to postgres://admin:hunter2@db.internal:5432/app", "connection_string"),
            ("set password = correcthorsebattery", "secret_assignment"),
            ("Authorization: Bearer abcdefghijklmnopqrstuvwxyz012345", "bearer_token"),
        ] {
            assert_eq!(find_secret(text), Some(kind), "{text}");
        }
        for text in [
            "use pnpm, not npm",
            "the tests need `cargo test --features sqlite`",
            "connect to postgres://db.internal:5432/app",
            "a token bucket limits the rate",
        ] {
            assert_eq!(find_secret(text), None, "{text}");
        }
    }

    #[test]
    fn personal_data_is_replaced() {
        assert_eq!(
            redact_pii("mail release notes to ops@example.com"),
            ("mail release notes to [EMAIL REDACTED]".to_string(), true)
        );
        assert_eq!(redact_pii("use pnpm"), ("use pnpm".to_string(), false));
    }
}
