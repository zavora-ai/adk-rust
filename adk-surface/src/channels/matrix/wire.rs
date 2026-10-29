//! What crosses the wire to and from Matrix (spec 3.5 §10 X12; spec 3.3 S-I4, S-I5, S-I6,
//! S-I9): room events read into the kernel's inbound values, and the kernel's cards and
//! notices drawn as `m.room.message` events with a plain `body` and an HTML `formatted_body`.
//!
//! Matrix has no buttons: a card lists its answers as reaction keys and names its decision in
//! a field of its own content, and only an `m.reaction` annotation on that card with one of
//! those keys answers it.

use adk_kernel::channels::{ChannelAnswer, ChannelInbound, DecisionCard, InboundBody};
use chrono::FixedOffset;
use serde_json::{Map, Value, json};

use super::MATRIX_CHANNEL;
use crate::channels::media::FileRef;
use crate::channels::shared::{
    Heading, Piece, Words, buttons, card_words, chunks, clean_inbound, truncate,
};

/// The field of a card's content that names its decision, version and answer keys; read back
/// only from the adapter's own events.
pub(crate) const CARD_FIELD: &str = "rs.adk.card";
/// The reaction that allows once.
pub(crate) const ALLOW_KEY: &str = "\u{1F44D}";
/// The reaction that denies.
pub(crate) const DENY_KEY: &str = "\u{1F44E}";
/// The longest piece of an agent's reply in one event.
const MAX_MESSAGE_CHARS: usize = 8000;
/// What an encrypted room is told, once.
pub(crate) const ENCRYPTED_ROOM: &str = "This room is encrypted, and your agents read only \
     unencrypted rooms. Start a direct chat with this account with encryption off to talk to \
     them.";

/// What a card answers: its decision, the version it showed, and its reaction keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CardRef {
    pub(crate) decision: String,
    pub(crate) version: u64,
    pub(crate) answers: Vec<(String, ChannelAnswer)>,
}

fn text_of(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// The reaction key of `answer`.
fn key_for(answer: ChannelAnswer) -> Option<&'static str> {
    match answer {
        ChannelAnswer::AllowOnce => Some(ALLOW_KEY),
        ChannelAnswer::Deny => Some(DENY_KEY),
        _ => None,
    }
}

/// `key` as the adapter compares it: without the variation selectors and skin tones clients
/// add to an emoji.
pub(crate) fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|c| !matches!(c, '\u{FE0E}' | '\u{FE0F}' | '\u{1F3FB}'..='\u{1F3FF}'))
        .collect()
}

/// A new text message from `room` (S-I6): edits, notices, the adapter's own events and other
/// message types are not; a reply's quoted fallback is dropped.
pub(crate) fn typed(room: &str, event: &Value, me: Option<&str>) -> Option<ChannelInbound> {
    let content = &event["content"];
    let sender = text_of(&event["sender"]);
    let id = text_of(&event["event_id"]);
    let image = content["msgtype"] == "m.image";
    if event["type"] != "m.room.message"
        || (content["msgtype"] != "m.text" && !image)
        || content["m.relates_to"]["rel_type"] == "m.replace"
        || content.get("m.new_content").is_some()
        || sender.is_empty()
        || id.is_empty()
        || room.is_empty()
        || me == Some(sender)
    {
        return None;
    }
    let mut body = text_of(&content["body"]);
    if content["m.relates_to"].get("m.in_reply_to").is_some() {
        // The quoted lines a client puts before a reply, then a blank line.
        let quoted: usize = body
            .split_inclusive('\n')
            .take_while(|line| line.starts_with("> "))
            .map(str::len)
            .sum();
        body = body[quoted..].trim_start_matches('\n');
    }
    if image {
        // An image's body is its caption only when a separate file name is given.
        let caption = content["filename"].as_str().filter(|name| *name != body).map(|_| body);
        let text = caption.and_then(clean_inbound).unwrap_or_default();
        return Some(ChannelInbound::text(MATRIX_CHANNEL, sender, room, id, text));
    }
    let text = clean_inbound(body)?;
    Some(ChannelInbound::text(MATRIX_CHANNEL, sender, room, id, text))
}

/// The image an `m.image` event names, by its `mxc://` URI (spec 3.5 §10 X12).
pub(crate) fn image(event: &Value) -> Option<FileRef> {
    let content = &event["content"];
    if event["type"] != "m.room.message" || content["msgtype"] != "m.image" {
        return None;
    }
    let url = content["url"].as_str().filter(|url| url.starts_with("mxc://"))?;
    let name =
        content["filename"].as_str().or_else(|| content["body"].as_str()).unwrap_or_default();
    Some(FileRef {
        url: url.to_string(),
        media_type: content["info"]["mimetype"].as_str().map(str::to_string),
        name: name.to_string(),
        size: content["info"]["size"].as_u64(),
    })
}

/// The card event and key a reaction annotates, when it is one.
pub(crate) fn reaction(event: &Value) -> Option<(String, String)> {
    let relates = &event["content"]["m.relates_to"];
    if event["type"] != "m.reaction" || relates["rel_type"] != "m.annotation" {
        return None;
    }
    let (card, key) = (text_of(&relates["event_id"]), text_of(&relates["key"]));
    (!card.is_empty() && !key.is_empty()).then(|| (card.to_string(), key.to_string()))
}

/// What `content`, the content of one of the adapter's own cards, answers.
pub(crate) fn card_ref(content: &Value) -> Option<CardRef> {
    let field = &content[CARD_FIELD];
    let decision = field["decision"].as_str().filter(|decision| !decision.is_empty())?;
    let version = field["version"].as_u64()?;
    let answers = field["answers"]
        .as_object()?
        .iter()
        .filter_map(|(key, answer)| {
            Some((key.clone(), serde_json::from_value(answer.clone()).ok()?))
        })
        .collect();
    Some(CardRef { decision: decision.to_string(), version, answers })
}

/// An answer (S-I4): a reaction from `room` on a card, with one of the keys that card offers;
/// `None` for any other key.
pub(crate) fn answer(room: &str, event: &Value, card: &CardRef) -> Option<ChannelInbound> {
    let (_, key) = reaction(event)?;
    let key = normalize_key(&key);
    let sender = text_of(&event["sender"]);
    let id = text_of(&event["event_id"]);
    if sender.is_empty() || id.is_empty() || room.is_empty() {
        return None;
    }
    let answer = card.answers.iter().find(|(offered, _)| normalize_key(offered) == key)?.1;
    let mut inbound = ChannelInbound::text(MATRIX_CHANNEL, sender, room, id, "");
    inbound.body =
        InboundBody::Answer { decision: card.decision.clone(), version: card.version, answer };
    Some(inbound)
}

/// Text for HTML: `&`, `<`, `>`, `"` and `'` escaped (S-I9).
pub(crate) fn html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// `words` as a message: the plain `body`, and a `formatted_body` with every piece escaped.
pub(crate) fn message(words: &Words) -> Value {
    let formatted = words.render(|piece| match piece {
        Piece::Plain(text) => html(text),
        Piece::Bold(text) => format!("<b>{}</b>", html(text)),
        Piece::Code(text) => format!("<code>{}</code>", html(text)),
        Piece::Link { url, label } => format!("<a href=\"{}\">{}</a>", html(url), html(label)),
    });
    json!({
        "msgtype": "m.text",
        "body": words.text(),
        "format": "org.matrix.custom.html",
        "formatted_body": formatted.replace('\n', "<br>"),
    })
}

/// A decision card (spec 3.3 §5.3): the chat screen's lines, its answers as reaction keys —
/// "To answer, react 👍 Allow once · 👎 Deny" — and "Open in home". The decision's id and
/// version ride in the card's own content field, never in the text (§8). Returns the card's
/// content and what it answers, `None` when it offers no answer.
pub(crate) fn card(
    card: &DecisionCard,
    heading: &Heading,
    offset: Option<FixedOffset>,
) -> (Value, Option<CardRef>) {
    let words = card_words(card, heading, offset);
    let offered: Vec<(String, ChannelAnswer, &str)> = buttons(card)
        .into_iter()
        .filter_map(|button| {
            Some((key_for(button.answer)?.to_string(), button.answer, button.label))
        })
        .collect();
    let mut lines = words.without_link();
    if !offered.is_empty() {
        let keys: Vec<String> =
            offered.iter().map(|(key, _, label)| format!("{key} {label}")).collect();
        lines.lines.push(vec![Piece::Plain(format!("To answer, react {}", keys.join(" · ")))]);
    }
    if let Some((url, label)) = words.link() {
        lines.lines.push(vec![Piece::Link { url: url.to_string(), label: label.to_string() }]);
    }
    let mut content = message(&lines);
    if offered.is_empty() {
        return (content, None);
    }
    let answers: Map<String, Value> = offered
        .iter()
        .map(|(key, answer, _)| (key.clone(), serde_json::to_value(answer).unwrap_or(Value::Null)))
        .collect();
    content[CARD_FIELD] =
        json!({ "decision": card.decision, "version": card.version, "answers": answers });
    let answered = CardRef {
        decision: card.decision.clone(),
        version: card.version,
        answers: offered.into_iter().map(|(key, answer, _)| (key, answer)).collect(),
    };
    (content, Some(answered))
}

/// An agent's text as Matrix shows it: a plain `body` with no `formatted_body`, so nothing in
/// it is read as markup (S-I9), in events of at most 8000 characters. Empty text sends
/// nothing.
pub(crate) fn plain(text: &str) -> Vec<Value> {
    chunks(text, MAX_MESSAGE_CHARS)
        .into_iter()
        .map(|chunk| json!({ "msgtype": "m.text", "body": truncate(&chunk, MAX_MESSAGE_CHARS) }))
        .collect()
}

/// The adapter's own reaction with `key` on its card `event`, so the person has only to tap.
pub(crate) fn offer(event: &str, key: &str) -> Value {
    json!({ "m.relates_to": { "rel_type": "m.annotation", "event_id": event, "key": key } })
}

#[cfg(test)]
mod tests {
    use adk_core::{
        ActingContext, AgentPrincipal, ApprovalPolicy, ApprovalRequest, ApprovalRisk,
        ApprovalScope, ApproverSelector, ToolConfirmationRequest,
    };

    use super::*;
    use crate::channels::shared::heading;

    fn request(summary: &str, risk: ApprovalRisk) -> ApprovalRequest {
        let call = ToolConfirmationRequest::new("send_email", json!({ "to": "a@example.com" }));
        ApprovalRequest::new(
            "apr_1",
            ApprovalScope::new("adk-rust", "ana", "s1"),
            &call,
            ApprovalPolicy::new([ApproverSelector::group("operator")]),
        )
        .with_summary(summary)
        .with_risk(risk)
        .with_acting(ActingContext::new(AgentPrincipal::derived("adk-rust", "finance")))
    }

    fn text_event(body: &str) -> Value {
        json!({ "type": "m.room.message", "sender": "@ana:hs", "event_id": "$e1",
                "content": { "msgtype": "m.text", "body": body } })
    }

    #[test]
    fn a_text_message_is_typed_and_everything_else_is_ignored() {
        let inbound = typed("!r:hs", &text_event(" hi\u{200B} there "), Some("@bot:hs")).unwrap();
        assert_eq!(
            inbound,
            ChannelInbound::text(MATRIX_CHANNEL, "@ana:hs", "!r:hs", "$e1", "hi there")
        );
        assert!(typed("!r:hs", &text_event("x"), Some("@ana:hs")).is_none(), "the adapter's own");
        let mut notice = text_event("x");
        notice["content"]["msgtype"] = json!("m.notice");
        assert!(typed("!r:hs", &notice, None).is_none());
        let mut edit = text_event("* stop all");
        edit["content"]["m.relates_to"] = json!({ "rel_type": "m.replace", "event_id": "$e0" });
        assert!(typed("!r:hs", &edit, None).is_none());
        let mut reply = text_event("> <@bot:hs> Waiting for you\n> more\n\nstop all");
        reply["content"]["m.relates_to"] = json!({ "m.in_reply_to": { "event_id": "$c" } });
        assert_eq!(
            typed("!r:hs", &reply, None).unwrap().body,
            InboundBody::Text { text: "stop all".into() }
        );
    }

    #[test]
    fn only_a_reaction_with_a_key_the_card_offers_answers() {
        let card_value = DecisionCard::from_request(&request("Send it", ApprovalRisk::Low), None);
        let (content, answered) =
            card(&card_value, &heading(&card_value, None), FixedOffset::east_opt(0));
        let answered = answered.unwrap();
        assert_eq!(card_ref(&content), Some(answered.clone()));
        assert!(
            content["body"]
                .as_str()
                .unwrap()
                .contains("To answer, react \u{1F44D} Allow once · \u{1F44E} Deny")
        );
        let react = |key: &str| {
            json!({ "type": "m.reaction", "sender": "@ana:hs", "event_id": "$r1",
                    "content": { "m.relates_to": { "rel_type": "m.annotation", "event_id": "$card", "key": key } } })
        };
        let inbound = answer("!r:hs", &react("\u{1F44D}\u{FE0F}"), &answered).unwrap();
        assert_eq!(
            inbound.body,
            InboundBody::Answer {
                decision: "apr_1".into(),
                version: card_value.version,
                answer: ChannelAnswer::AllowOnce
            }
        );
        assert!(answer("!r:hs", &react("\u{1F44E}\u{1F3FD}"), &answered).is_some(), "a skin tone");
        assert!(answer("!r:hs", &react("\u{2764}"), &answered).is_none(), "an unknown key");
        assert_eq!(reaction(&react("x")), Some(("$card".into(), "x".into())));
        // A high-risk card offers no key and names no decision.
        let high = DecisionCard::from_request(&request("Send it", ApprovalRisk::High), None);
        let (content, answered) = card(&high, &heading(&high, None), None);
        assert!(answered.is_none() && content.get(CARD_FIELD).is_none());
    }

    #[test]
    fn what_is_shown_is_escaped() {
        let card_value = DecisionCard::from_request(
            &request("Run <script>x</script> & \"y\"", ApprovalRisk::Low),
            Some("https://home.example/ui/"),
        );
        let (content, _) = card(&card_value, &heading(&card_value, None), None);
        let formatted = content["formatted_body"].as_str().unwrap();
        assert!(
            formatted.contains("<b>Run &lt;script&gt;x&lt;/script&gt; &amp; &quot;y&quot;</b>"),
            "{formatted}"
        );
        assert!(
            formatted.ends_with("<a href=\"https://home.example/ui/\">Open in home</a>"),
            "{formatted}"
        );
        assert!(!content.to_string().contains("a@example.com"));
        assert_eq!(plain("<b>hi</b>")[0], json!({ "msgtype": "m.text", "body": "<b>hi</b>" }));
    }
}
