//! What crosses the wire to and from WhatsApp (spec 3.5 §10 X12; spec 3.3 S-I4, S-I5, S-I6,
//! S-I9): the Cloud API's webhook payloads read into the kernel's inbound values, and the
//! kernel's cards and notices drawn as text and interactive reply-button messages.

use adk_kernel::channels::{ChannelInbound, DecisionCard, InboundBody};
use chrono::FixedOffset;
use serde_json::{Value, json};

use super::WHATSAPP_CHANNEL;
use crate::channels::shared::{
    Button, Heading, Piece, Words, buttons, card_words, chunks, clean_inbound, parse_callback,
    truncate,
};

/// WhatsApp's limit on an interactive message's reply buttons.
pub(crate) const MAX_BUTTONS: usize = 3;
/// WhatsApp's limit on a reply button's title, in characters.
pub(crate) const MAX_TITLE_CHARS: usize = 20;
/// WhatsApp's limit on an interactive message's body, in characters.
const MAX_BODY_CHARS: usize = 1024;
/// WhatsApp's limit on a text message's body, in characters; replies leave room for escapes.
const MAX_TEXT_CHARS: usize = 4096;
const MAX_REPLY_CHARS: usize = 3000;
/// What WhatsApp shows as nothing, put on both sides of a formatting marker so it formats
/// nothing.
const BREAK: char = '\u{200B}';

/// One message the webhook carried.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Received {
    /// The message's id, `wamid.…`, by which a replay is known.
    pub(crate) id: String,
    /// The conversation: the customer's WhatsApp id, which replies go to.
    pub(crate) thread: String,
    /// When it was sent, in seconds since the epoch, as WhatsApp gave it.
    pub(crate) at: Option<i64>,
    /// What the gateway is given, when anything.
    pub(crate) inbound: Option<ChannelInbound>,
    /// The card a pressed button was on.
    pub(crate) card: Option<String>,
}

fn text_of(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// Reads one webhook payload for the business number `phone_number_id` (S-I6). A text
/// message becomes a typed message, a pressed reply button an answer; every message, of any
/// type, is returned so it opens the customer-service window. Messages to another number,
/// status updates and other fields are not read.
pub(crate) fn parse(payload: &Value, phone_number_id: &str) -> Vec<Received> {
    let mut received = Vec::new();
    if payload["object"] != "whatsapp_business_account" {
        return received;
    }
    let changes = payload["entry"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|entry| entry["changes"].as_array().into_iter().flatten());
    for change in changes {
        let value = &change["value"];
        if change["field"] != "messages" || value["metadata"]["phone_number_id"] != phone_number_id
        {
            continue;
        }
        let contacts: Vec<&Value> = value["contacts"].as_array().into_iter().flatten().collect();
        for message in value["messages"].as_array().into_iter().flatten() {
            let (from, id) = (text_of(&message["from"]), text_of(&message["id"]));
            if from.is_empty() || id.is_empty() {
                continue;
            }
            // Replies go to the contact's WhatsApp id, as the Cloud API asks.
            let contact = match contacts.as_slice() {
                [only] => Some(*only),
                _ => contacts.iter().copied().find(|contact| contact["wa_id"] == from),
            };
            let thread = contact.map_or(from, |contact| text_of(&contact["wa_id"]));
            let thread = if thread.is_empty() { from } else { thread };
            let name = contact.and_then(|contact| contact["profile"]["name"].as_str());
            let (inbound, card) = read(message, from, thread, id);
            let inbound = match (inbound, name.filter(|name| !name.is_empty())) {
                (Some(inbound), Some(name)) => Some(inbound.with_display_name(name)),
                (inbound, _) => inbound,
            };
            received.push(Received {
                id: id.to_string(),
                thread: thread.to_string(),
                at: text_of(&message["timestamp"]).parse().ok(),
                inbound,
                card,
            });
        }
    }
    received
}

/// A message's text, or its pressed reply button: only a button answers a decision (S-I4).
fn read(
    message: &Value,
    from: &str,
    thread: &str,
    id: &str,
) -> (Option<ChannelInbound>, Option<String>) {
    match message["type"].as_str() {
        Some("text") => {
            let inbound = clean_inbound(text_of(&message["text"]["body"]))
                .map(|text| ChannelInbound::text(WHATSAPP_CHANNEL, from, thread, id, text));
            (inbound, None)
        }
        Some("interactive") if message["interactive"]["type"] == "button_reply" => {
            let reply = &message["interactive"]["button_reply"];
            let Some((answer, version, decision)) = reply["id"].as_str().and_then(parse_callback)
            else {
                return (None, None);
            };
            let card = message["context"]["id"].as_str().map(str::to_string);
            let mut inbound = ChannelInbound::text(WHATSAPP_CHANNEL, from, thread, id, "");
            inbound.body = InboundBody::Answer { decision, version, answer };
            (Some(inbound), card)
        }
        _ => (None, None),
    }
}

/// Text for WhatsApp's markup (S-I9): `*`, `_`, `~` and `` ` `` between zero-width spaces, and
/// a quote or list marker at a line's start after one, so nothing formats. WhatsApp has no
/// escape character of its own.
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut line_start = true;
    for c in text.chars() {
        match c {
            '*' | '_' | '~' | '`' => {
                out.push(BREAK);
                out.push(c);
                out.push(BREAK);
            }
            '>' | '-' if line_start => {
                out.push(BREAK);
                out.push(c);
            }
            c => out.push(c),
        }
        line_start = c == '\n' || (line_start && c == ' ');
    }
    out
}

/// `words` in WhatsApp's markup: plain text escaped, a heading in bold, a pairing command as
/// monospace, a link as its address, which WhatsApp makes a link.
fn render(words: &Words) -> String {
    words.render(|piece| match piece {
        Piece::Plain(text) => escape(text),
        Piece::Bold(text) => format!("*{}*", escape(text)),
        Piece::Code(text) => format!("```{}```", text.replace('`', "")),
        Piece::Link { url, label } => format!("{label}: {url}"),
    })
}

/// A text message to `to`, replying to `reply_to` when set.
pub(crate) fn text(to: &str, body: &str, reply_to: Option<&str>) -> Value {
    let mut message = json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": to,
        "type": "text",
        "text": { "body": truncate(body, MAX_TEXT_CHARS), "preview_url": false },
    });
    if let Some(id) = reply_to {
        message["context"] = json!({ "message_id": id });
    }
    message
}

/// A notice to `to` in the chat screen's words.
pub(crate) fn message(to: &str, words: &Words, reply_to: Option<&str>) -> Value {
    text(to, &render(words), reply_to)
}

/// The reply buttons of `card`: at most three, each title within twenty characters; `None`
/// when the card offers none or more than WhatsApp shows.
fn reply_buttons(card: &DecisionCard) -> Option<Vec<Button>> {
    let buttons = buttons(card);
    (!buttons.is_empty() && buttons.len() <= MAX_BUTTONS).then_some(buttons)
}

/// A decision card (spec 3.3 §5.3): the chat screen's lines with "Open in home" as its
/// address, and a reply button for each answer the card offers and no others. The decision's
/// id and version ride in the buttons' ids, never in the text (§8). A card WhatsApp cannot
/// draw with buttons is sent as text, answered in the home.
pub(crate) fn card(
    to: &str,
    card: &DecisionCard,
    heading: &Heading,
    offset: Option<FixedOffset>,
) -> Value {
    let words = card_words(card, heading, offset);
    let Some(buttons) = reply_buttons(card) else {
        let mut words = words;
        // More answers than WhatsApp shows: the card is answered in the home.
        if buttons(card).len() > MAX_BUTTONS {
            let decide = adk_kernel::channels::ChannelNotice::DecideInHome { home: None }.text();
            let at = words.lines.len() - usize::from(words.link().is_some());
            words.lines.insert(at, vec![Piece::Plain(decide)]);
        }
        return text(to, &render(&words), None);
    };
    let buttons: Vec<Value> = buttons
        .into_iter()
        .map(|button| {
            json!({ "type": "reply", "reply": { "id": button.data, "title": truncate(button.label, MAX_TITLE_CHARS) } })
        })
        .collect();
    json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": to,
        "type": "interactive",
        "interactive": {
            "type": "button",
            "body": { "text": truncate(&render(&words), MAX_BODY_CHARS) },
            "action": { "buttons": buttons },
        },
    })
}

/// An agent's text as WhatsApp shows it: escaped, never formatted (S-I9), in messages within
/// WhatsApp's length. Empty text sends nothing.
pub(crate) fn plain(to: &str, text_body: &str) -> Vec<Value> {
    chunks(text_body, MAX_REPLY_CHARS)
        .into_iter()
        .map(|chunk| text(to, &escape(&chunk), None))
        .collect()
}

/// The approved template `name` in `language`, the only message WhatsApp delivers outside the
/// customer-service window.
pub(crate) fn template(to: &str, name: &str, language: &str) -> Value {
    json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": to,
        "type": "template",
        "template": { "name": name, "language": { "code": language } },
    })
}

#[cfg(test)]
mod tests {
    use adk_core::{
        ActingContext, AgentPrincipal, ApprovalPolicy, ApprovalRequest, ApprovalRisk,
        ApprovalScope, ApproverSelector, ToolConfirmationRequest,
    };
    use adk_kernel::channels::ChannelAnswer;
    use proptest::prelude::*;

    use super::*;
    use crate::channels::shared::heading;

    fn payload(message: Value) -> Value {
        json!({ "object": "whatsapp_business_account", "entry": [{ "id": "w1", "changes": [{
            "field": "messages",
            "value": {
                "messaging_product": "whatsapp",
                "metadata": { "display_phone_number": "15550000000", "phone_number_id": "1001" },
                "contacts": [{ "profile": { "name": "Ana" }, "wa_id": "254700000001" }],
                "messages": [message],
            },
        }]}]})
    }

    #[test]
    fn a_text_message_is_typed_and_a_reply_button_answers() {
        let text_message = json!({ "from": "254700000001", "id": "wamid.1", "timestamp": "1761300000",
            "type": "text", "text": { "body": " hi\u{200B} there " } });
        let received = parse(&payload(text_message), "1001");
        assert_eq!(received.len(), 1);
        assert_eq!(
            received[0].inbound,
            Some(
                ChannelInbound::text(
                    WHATSAPP_CHANNEL,
                    "254700000001",
                    "254700000001",
                    "wamid.1",
                    "hi there"
                )
                .with_display_name("Ana")
            )
        );
        assert_eq!(received[0].at, Some(1_761_300_000));
        let press = json!({ "from": "254700000001", "id": "wamid.2", "timestamp": "1761300001",
            "type": "interactive", "context": { "from": "15550000000", "id": "wamid.card" },
            "interactive": { "type": "button_reply", "button_reply": { "id": "y:4:apr_1", "title": "Allow once" } } });
        let received = parse(&payload(press), "1001");
        assert_eq!(
            received[0].inbound.as_ref().unwrap().body,
            InboundBody::Answer {
                decision: "apr_1".into(),
                version: 4,
                answer: ChannelAnswer::AllowOnce
            }
        );
        assert_eq!(received[0].card.as_deref(), Some("wamid.card"));
        // Typed words never answer; another number's messages are not read; media opens the
        // window and reaches no agent.
        let typed = json!({ "from": "254700000001", "id": "wamid.3", "type": "text", "text": { "body": "y:4:apr_1" } });
        assert!(matches!(
            parse(&payload(typed.clone()), "1001")[0].inbound.as_ref().unwrap().body,
            InboundBody::Text { .. }
        ));
        assert!(parse(&payload(typed), "2002").is_empty());
        let image = json!({ "from": "254700000001", "id": "wamid.4", "type": "image", "image": { "id": "m1" } });
        let received = parse(&payload(image), "1001");
        assert!(received[0].inbound.is_none());
        assert!(parse(&json!({ "object": "page" }), "1001").is_empty());
    }

    fn request(risk: ApprovalRisk) -> ApprovalRequest {
        let call = ToolConfirmationRequest::new("send_email", json!({ "to": "a@example.com" }));
        ApprovalRequest::new(
            "apr_1",
            ApprovalScope::new("adk-rust", "ana", "s1"),
            &call,
            ApprovalPolicy::new([ApproverSelector::group("operator")]),
        )
        .with_summary("Send *the* _invoice_ to a@example.com")
        .with_risk(risk)
        .with_acting(ActingContext::new(AgentPrincipal::derived("adk-rust", "finance")))
    }

    #[test]
    fn a_card_has_at_most_three_buttons_of_twenty_characters() {
        let card_value = DecisionCard::from_request(
            &request(ApprovalRisk::Low),
            Some("https://home.example/ui/"),
        );
        let drawn = card(
            "254700000001",
            &card_value,
            &heading(&card_value, None),
            FixedOffset::east_opt(0),
        );
        let buttons = drawn["interactive"]["action"]["buttons"].as_array().unwrap();
        assert!(buttons.len() <= MAX_BUTTONS);
        for button in buttons {
            assert!(button["reply"]["title"].as_str().unwrap().chars().count() <= MAX_TITLE_CHARS);
            assert!(button["reply"]["id"].as_str().unwrap().len() <= 256);
        }
        let body = drawn["interactive"]["body"]["text"].as_str().unwrap();
        assert!(body.contains("\u{200B}*\u{200B}the\u{200B}*\u{200B}"), "{body}");
        assert!(body.ends_with("Open in home: https://home.example/ui/"), "{body}");
        assert!(!drawn.to_string().contains("a@example.com"));
        let high = DecisionCard::from_request(&request(ApprovalRisk::High), None);
        let drawn = card("254700000001", &high, &heading(&high, None), None);
        assert_eq!(drawn["type"], "text", "no buttons at high risk");
        assert_eq!(
            truncate("Allow once for all of this week", MAX_TITLE_CHARS).chars().count(),
            MAX_TITLE_CHARS
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Escaped text reads as the text, and no marker in it touches visible text.
        #[test]
        fn escaping_reads_back_and_breaks_every_marker(text in "\\PC{0,200}") {
            let escaped = escape(&text);
            prop_assert_eq!(escaped.replace(BREAK, ""), text.replace(BREAK, ""));
            let chars: Vec<char> = escaped.chars().collect();
            for (index, c) in chars.iter().enumerate() {
                if matches!(c, '*' | '_' | '~' | '`') {
                    prop_assert_eq!(chars.get(index + 1), Some(&BREAK));
                    prop_assert_eq!(index.checked_sub(1).and_then(|i| chars.get(i)), Some(&BREAK));
                }
            }
        }
    }
}
