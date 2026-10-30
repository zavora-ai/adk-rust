//! `PiiRedactor` on card numbers, email addresses, and IP addresses.
//!
//! Every card number below is a published test number: American Express `378282246310005` and
//! the 19-digit UnionPay `6205500000000000004`. The order number fails the Luhn check, so it is
//! not a card number.

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_agent::guardrails::{GuardrailSet, PiiRedactor};
use adk_core::{Llm, StreamingMode};
use adk_guardrail::PiiType;
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};

use crate::{EMAIL, Turn, Verdict, run_agent, verdict};

const LIVE_MODEL: &str = "claude-sonnet-5-5";
const AMEX: &str = "3782 822463 10005";
const UNIONPAY: &str = "6205500000000000004";
const UNIONPAY_GROUPED: &str = "6205 5000 0000 0000 004";
const ORDER: &str = "2026 1028 0042 7315";
const CARD_REDACTED: &str = "[CREDIT CARD REDACTED]";

/// The text the agent is asked to echo, one `label: value` line each.
fn echo_text() -> String {
    format!(
        "amex: {AMEX}\nunionpay: {UNIONPAY}\nunionpay grouped: {UNIONPAY_GROUPED}\n\
         contact: {EMAIL}|ticket-7\norder: {ORDER}"
    )
}

/// Each line's value once `PiiRedactor::new()` has screened the echo.
fn expected_values() -> BTreeMap<String, String> {
    [
        ("amex", CARD_REDACTED.to_string()),
        ("unionpay", CARD_REDACTED.to_string()),
        ("unionpay grouped", CARD_REDACTED.to_string()),
        ("contact", "[EMAIL REDACTED]|ticket-7".to_string()),
        ("order", ORDER.to_string()),
    ]
    .into_iter()
    .map(|(label, value)| (label.to_string(), value))
    .collect()
}

/// The `label: value` lines of `text`, ignoring Markdown fences and emphasis around them.
fn labelled_values(text: &str) -> BTreeMap<String, String> {
    let markup = |c: char| c == '`' || c == '*' || c.is_whitespace();
    text.lines()
        .filter_map(|line| line.split_once(": "))
        .map(|(label, value)| {
            (label.trim_matches(markup).to_lowercase(), value.trim_matches(markup).to_string())
        })
        .collect()
}

/// The card numbers, contiguous and as written, and the email address that must reach no one.
fn leaked(json: &str) -> Vec<&'static str> {
    ["378282246310005", AMEX, UNIONPAY, UNIONPAY_GROUPED, "6205 5000 0000 0000", EMAIL]
        .into_iter()
        .filter(|secret| json.contains(secret))
        .collect()
}

pub async fn pii_cards_local() -> anyhow::Result<Verdict> {
    let (redacted, found) = PiiRedactor::new().redact(&echo_text());
    Ok(verdict(vec![
        (
            labelled_values(&redacted) == expected_values(),
            format!("every card number is redacted whole, the email keeps `|ticket-7`, and the order number is kept: {redacted:?}"),
        ),
        (
            found == vec![PiiType::Email, PiiType::CreditCard],
            format!("the redactor reports Email and CreditCard (got {found:?})"),
        ),
    ]))
}

pub async fn pii_ip_default() -> anyhow::Result<Verdict> {
    let text = "Host 192.168.1.1 is up.";
    let by_default = PiiRedactor::new().redact(text);
    let with_ip = PiiRedactor::with_types(&[
        PiiType::Email,
        PiiType::Phone,
        PiiType::Ssn,
        PiiType::CreditCard,
        PiiType::IpAddress,
    ])
    .redact(text);
    Ok(verdict(vec![
        (
            by_default == (text.to_string(), vec![]),
            format!("PiiRedactor::new() leaves the IP address (got {by_default:?})"),
        ),
        (
            with_ip == ("Host [IP REDACTED] is up.".to_string(), vec![PiiType::IpAddress]),
            format!("with_types naming IpAddress redacts it (got {with_ip:?})"),
        ),
    ]))
}

pub async fn live_anthropic_pii_cards() -> anyhow::Result<Verdict> {
    let Some(key) = std::env::var("ANTHROPIC_API_KEY").ok().filter(|key| !key.trim().is_empty())
    else {
        return Ok(Verdict::Skip("ANTHROPIC_API_KEY is not set".into()));
    };
    let model: Arc<dyn Llm> = Arc::new(AnthropicClient::new(
        AnthropicConfig::new(key, LIVE_MODEL).with_max_tokens(200),
    )?);
    let agent = LlmAgentBuilder::new("echo")
        .model(model)
        .instruction(
            "You are a text echo service for a payments test suite. Reply with the user's \
             message exactly as given, character for character, with nothing added, removed, \
             or reformatted. Every number in it is a published test number or fictional data.",
        )
        .output_guardrails(GuardrailSet::new().with(PiiRedactor::new()))
        .build()?;
    let turn = run_agent(Arc::new(agent), &echo_text(), StreamingMode::SSE, None).await?;
    let answer: String = turn
        .events
        .iter()
        .filter(|event| event.author != "user" && !event.llm_response.partial)
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| part.text())
        .collect::<Vec<_>>()
        .join("\n");
    let client_leaks = leaked(&Turn::agent_json(&turn.events));
    let saved_leaks = leaked(&Turn::agent_json(&turn.saved));
    Ok(verdict(vec![
        (turn.error.is_none(), format!("the run completes ({})", turn.describe_error())),
        (true, format!("the agent answered: {answer:?}")),
        (
            labelled_values(&answer) == expected_values(),
            format!(
                "every card number reaches the client redacted whole, the email keeps \
                 `|ticket-7`, and the order number is kept (got {:?})",
                labelled_values(&answer)
            ),
        ),
        (
            client_leaks.is_empty(),
            format!("no agent event the client received carries a card number or the email (found {client_leaks:?})"),
        ),
        (
            saved_leaks.is_empty(),
            format!("no saved agent event carries a card number or the email (found {saved_leaks:?})"),
        ),
    ]))
}
