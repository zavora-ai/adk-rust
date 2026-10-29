//! What crosses the wire to and from Slack (spec 3.5 §10 X12; spec 3.3 S-I4, S-I5, S-I6,
//! S-I9): Socket Mode envelopes read into the kernel's inbound values, and the kernel's cards
//! and notices drawn as Block Kit messages.

use adk_kernel::channels::{ChannelInbound, DecisionCard, InboundBody};
use chrono::FixedOffset;
use serde_json::{Value, json};

use super::SLACK_CHANNEL;
use crate::channels::media::FileRef;
use crate::channels::shared::{
    Heading, Piece, Words, buttons, card_words, chunks, clean_inbound, parse_callback, truncate,
};

/// Slack's limit on a `plain_text` object in a section, in characters.
const MAX_SECTION_CHARS: usize = 3000;
/// Slack's limit on a header block, in characters.
const MAX_HEADER_CHARS: usize = 150;
/// The longest piece of an agent's reply in one message.
pub(crate) const MAX_MESSAGE_CHARS: usize = 3000;
/// The `action_id` prefix of an answer button; the home link is `adk-home`.
const ANSWER_ACTION: &str = "adk-answer-";

/// A pressed button, and the card it was on.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Press {
    /// The card's conversation and timestamp, its id in Slack.
    pub(crate) card: Option<(String, String)>,
    /// The card's blocks, to draw it again without its answers.
    pub(crate) blocks: Value,
    /// The card's fallback text.
    pub(crate) text: String,
}

/// One envelope as the adapter reads it.
#[derive(Debug, Default)]
pub(crate) struct Parsed {
    pub(crate) inbound: Option<ChannelInbound>,
    pub(crate) press: Option<Press>,
    /// The files a message shared (spec 3.5 §10 X12).
    pub(crate) files: Vec<FileRef>,
}

/// Reads one Socket Mode envelope (S-I6). A new message in a direct message with the app, from
/// a person, becomes a typed message; a pressed answer button becomes an answer; `/stop` typed
/// in the direct message stops everything; everything else — channels, bots, edits, files,
/// other events — is ignored, so a card never reaches a thread other people read.
pub(crate) fn parse(envelope: &Value) -> Parsed {
    let payload = &envelope["payload"];
    match envelope["type"].as_str() {
        Some("events_api") => match typed(&payload["event"]) {
            Some((inbound, files)) => Parsed { inbound: Some(inbound), press: None, files },
            None => Parsed::default(),
        },
        Some("interactive") if payload["type"] == "block_actions" => pressed(payload),
        Some("slash_commands") => {
            Parsed { inbound: command(payload), press: None, files: Vec::new() }
        }
        _ => Parsed::default(),
    }
}

fn text_of(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

fn typed(event: &Value) -> Option<(ChannelInbound, Vec<FileRef>)> {
    let direct = event["type"] == "message" && event["channel_type"] == "im";
    // Edits, joins and the app's own messages carry a subtype or a bot id; a message that
    // shares files is `file_share`.
    let subtype = event.get("subtype").map(|subtype| subtype == "file_share");
    if !direct || subtype == Some(false) || event.get("bot_id").is_some() {
        return None;
    }
    let (user, channel, ts) =
        (text_of(&event["user"]), text_of(&event["channel"]), text_of(&event["ts"]));
    if user.is_empty() || channel.is_empty() || ts.is_empty() {
        return None;
    }
    let files: Vec<FileRef> = event["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|file| {
            let url =
                file["url_private_download"].as_str().or_else(|| file["url_private"].as_str())?;
            Some(FileRef {
                url: url.to_string(),
                media_type: file["mimetype"].as_str().map(str::to_string),
                name: text_of(&file["name"]).to_string(),
                size: file["size"].as_u64(),
            })
        })
        .collect();
    let text = match clean_inbound(&unescape(text_of(&event["text"]))) {
        Some(text) => text,
        None if !files.is_empty() => String::new(),
        None => return None,
    };
    Some((ChannelInbound::text(SLACK_CHANNEL, user, channel, ts, text), files))
}

/// A pressed answer: only a button's value answers a decision, never typed text (S-I4).
fn pressed(payload: &Value) -> Parsed {
    let user = text_of(&payload["user"]["id"]);
    let channel = payload["channel"]["id"]
        .as_str()
        .or_else(|| payload["container"]["channel_id"].as_str())
        .unwrap_or_default();
    let ts = text_of(&payload["container"]["message_ts"]);
    let press = Press {
        card: (!channel.is_empty() && !ts.is_empty())
            .then(|| (channel.to_string(), ts.to_string())),
        blocks: payload["message"]["blocks"].clone(),
        text: text_of(&payload["message"]["text"]).to_string(),
    };
    let action = payload["actions"].as_array().and_then(|actions| actions.first());
    let answer = action
        .filter(|action| text_of(&action["action_id"]).starts_with(ANSWER_ACTION))
        .and_then(|action| parse_callback(action["value"].as_str()?));
    let Some((answer, version, decision)) = answer else {
        // The home link and foreign buttons answer nothing.
        return Parsed::default();
    };
    if user.is_empty() || channel.is_empty() {
        return Parsed::default();
    }
    let at = action.map_or("", |action| text_of(&action["action_ts"]));
    let mut inbound = ChannelInbound::text(SLACK_CHANNEL, user, channel, format!("act-{at}"), "");
    inbound.body = InboundBody::Answer { decision, version, answer };
    Parsed { inbound: Some(inbound), press: Some(press), files: Vec::new() }
}

/// `/stop` typed in the direct message with the app: "Stop all" (S-I10).
/// `/pair` there asks for a pairing code, as typing it does elsewhere (spec 3.5 §10 X12).
fn command(payload: &Value) -> Option<ChannelInbound> {
    let direct = payload["channel_name"] == "directmessage";
    let (user, channel) = (text_of(&payload["user_id"]), text_of(&payload["channel_id"]));
    if !direct || user.is_empty() || channel.is_empty() {
        return None;
    }
    let id = format!("cmd-{}", text_of(&payload["trigger_id"]));
    match payload["command"].as_str() {
        Some("/stop") => Some(ChannelInbound::stop_all(SLACK_CHANNEL, user, channel, id)),
        Some("/pair") => Some(ChannelInbound::text(SLACK_CHANNEL, user, channel, id, "/pair")),
        _ => None,
    }
}

/// Slack's own escapes in a message's text, read back as the person typed them.
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// Text for Slack's markup: `&`, `<` and `>` escaped, so nothing in it is read as a link, a
/// mention or a command (S-I9).
pub(crate) fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn plain_text(text: &str, max: usize) -> Value {
    json!({ "type": "plain_text", "text": truncate(text, max), "emoji": false })
}

/// The lines of `words` as blocks: a heading as a header, every other line as plain text,
/// which Slack never reads as markup (S-I9).
fn blocks(words: &Words) -> Vec<Value> {
    words
        .without_link()
        .lines
        .iter()
        .map(|line| match line.as_slice() {
            [Piece::Bold(title)] => {
                json!({ "type": "header", "text": plain_text(title, MAX_HEADER_CHARS) })
            }
            _ => {
                let text = Words { lines: vec![line.clone()] }.text();
                json!({ "type": "section", "text": plain_text(&text, MAX_SECTION_CHARS) })
            }
        })
        .collect()
}

/// "Open in home" as a link button.
fn home_button(words: &Words) -> Option<Value> {
    let (url, label) = words.link()?;
    Some(json!({
        "type": "button",
        "action_id": "adk-home",
        "text": plain_text(label, 75),
        "url": url,
    }))
}

/// A decision card (spec 3.3 §5.3): the chat screen's lines, a button for each answer the
/// card offers and no others, and "Open in home". The decision's id and version ride in the
/// buttons' values, never in the text (§8).
pub(crate) fn card(card: &DecisionCard, heading: &Heading, offset: Option<FixedOffset>) -> Value {
    let words = card_words(card, heading, offset);
    let mut elements: Vec<Value> = buttons(card)
        .into_iter()
        .enumerate()
        .map(|(index, button)| {
            json!({
                "type": "button",
                "action_id": format!("{ANSWER_ACTION}{index}"),
                "text": plain_text(button.label, 75),
                "value": button.data,
            })
        })
        .collect();
    elements.extend(home_button(&words));
    let mut blocks = blocks(&words);
    if !elements.is_empty() {
        blocks.push(json!({ "type": "actions", "block_id": "adk-card", "elements": elements }));
    }
    json!({
        "text": escape(&truncate(&words.without_link().text(), MAX_SECTION_CHARS)),
        "blocks": blocks,
        "unfurl_links": false,
        "unfurl_media": false,
    })
}

/// A notice: plain text with Slack's escapes, a pairing command as code, and "Open in home"
/// as a link button.
pub(crate) fn message(words: &Words) -> Value {
    let lines = words.without_link();
    let code = lines.lines.iter().flatten().any(|piece| matches!(piece, Piece::Code(_)));
    let text = lines.render(|piece| match piece {
        Piece::Plain(text) | Piece::Bold(text) => escape(text),
        Piece::Code(text) => format!("`{}`", escape(text)),
        Piece::Link { .. } => String::new(),
    });
    let mut body = json!({
        "text": text,
        "mrkdwn": code,
        "unfurl_links": false,
        "unfurl_media": false,
    });
    if let Some(button) = home_button(words) {
        body["blocks"] = json!([
            { "type": "section", "text": plain_text(&lines.text(), MAX_SECTION_CHARS) },
            { "type": "actions", "block_id": "adk-home", "elements": [button] },
        ]);
    }
    body
}

/// An agent's text as Slack shows it: escaped, never read as markup (S-I9), in messages of
/// at most 3000 characters. Empty text sends nothing.
pub(crate) fn plain(text: &str) -> Vec<Value> {
    chunks(text, MAX_MESSAGE_CHARS)
        .into_iter()
        .map(|chunk| {
            json!({ "text": escape(&chunk), "mrkdwn": false, "unfurl_links": false, "unfurl_media": false })
        })
        .collect()
}

/// The card `press` was on, drawn again without its answer buttons, so it is not pressed
/// again; "Open in home" stays.
pub(crate) fn answered(press: &Press) -> Value {
    let blocks: Vec<Value> = press
        .blocks
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| {
            if block["type"] != "actions" {
                return Some(block.clone());
            }
            let elements: Vec<Value> = block["elements"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|element| !text_of(&element["action_id"]).starts_with(ANSWER_ACTION))
                .cloned()
                .collect();
            let mut block = block.clone();
            block["elements"] = json!(elements);
            (!elements.is_empty()).then_some(block)
        })
        .collect();
    json!({ "text": press.text, "blocks": blocks })
}

#[cfg(test)]
mod tests {
    use adk_core::{
        ActingContext, AgentPrincipal, ApprovalPolicy, ApprovalRequest, ApprovalRisk,
        ApprovalScope, ApproverSelector, ToolConfirmationRequest,
    };
    use adk_kernel::channels::ChannelAnswer;

    use super::*;
    use crate::channels::shared::heading;

    fn message_event(text: &str) -> Value {
        json!({
            "type": "events_api",
            "envelope_id": "e1",
            "payload": { "type": "event_callback", "event": {
                "type": "message", "channel_type": "im", "user": "U1", "channel": "D1",
                "ts": "1.1", "text": text,
            }},
        })
    }

    #[test]
    fn a_direct_message_is_typed_and_everything_else_is_ignored() {
        let inbound = parse(&message_event(" 2 &lt; 3 &amp;\u{200B} ok ")).inbound.unwrap();
        assert_eq!(inbound, ChannelInbound::text(SLACK_CHANNEL, "U1", "D1", "1.1", "2 < 3 & ok"));
        let mut channel = message_event("stop all");
        channel["payload"]["event"]["channel_type"] = json!("channel");
        assert!(parse(&channel).inbound.is_none());
        let mut edit = message_event("stop all");
        edit["payload"]["event"]["subtype"] = json!("message_changed");
        assert!(parse(&edit).inbound.is_none());
        let mut bot = message_event("stop all");
        bot["payload"]["event"]["bot_id"] = json!("B1");
        assert!(parse(&bot).inbound.is_none());
        assert!(parse(&json!({ "type": "hello" })).inbound.is_none());
    }

    #[test]
    fn only_a_pressed_answer_button_answers_and_stop_is_a_command() {
        let press = |action_id: &str, value: &str| {
            json!({ "type": "interactive", "envelope_id": "e2", "payload": {
                "type": "block_actions",
                "user": { "id": "U1" },
                "channel": { "id": "D1" },
                "container": { "type": "message", "message_ts": "9.9", "channel_id": "D1" },
                "message": { "text": "card", "blocks": [] },
                "actions": [{ "action_id": action_id, "value": value, "action_ts": "10.1" }],
            }})
        };
        let parsed = parse(&press("adk-answer-0", "y:3:apr_1"));
        let inbound = parsed.inbound.unwrap();
        assert_eq!(
            inbound.body,
            InboundBody::Answer {
                decision: "apr_1".into(),
                version: 3,
                answer: ChannelAnswer::AllowOnce
            }
        );
        assert_eq!(inbound.message_id, "act-10.1");
        assert_eq!(parsed.press.unwrap().card, Some(("D1".into(), "9.9".into())));
        for (action, value) in
            [("adk-home", "y:3:apr_1"), ("adk-answer-0", "z:3:apr_1"), ("other", "n:1:a")]
        {
            assert!(parse(&press(action, value)).inbound.is_none(), "{action} {value}");
        }
        let typed = parse(&message_event("y:3:apr_1")).inbound.unwrap();
        assert!(matches!(typed.body, InboundBody::Text { .. }));
        let stop = json!({ "type": "slash_commands", "payload": {
            "command": "/stop", "channel_name": "directmessage", "user_id": "U1",
            "channel_id": "D1", "trigger_id": "t1",
        }});
        assert_eq!(parse(&stop).inbound.unwrap().body, InboundBody::StopAll);
        let mut pair = stop.clone();
        pair["payload"]["command"] = json!("/pair");
        assert_eq!(parse(&pair).inbound.unwrap().body, InboundBody::Text { text: "/pair".into() });
        let mut public = stop.clone();
        public["payload"]["channel_name"] = json!("general");
        assert!(parse(&public).inbound.is_none());
    }

    #[test]
    fn a_card_offers_its_answers_as_buttons_and_escapes_what_it_shows() {
        let call = ToolConfirmationRequest::new("send_email", json!({ "to": "a@example.com" }));
        let request = ApprovalRequest::new(
            "apr_1",
            ApprovalScope::new("adk-rust", "ana", "s1"),
            &call,
            ApprovalPolicy::new([ApproverSelector::group("operator")]),
        )
        .with_summary("Run <!channel> & <https://evil.example|click> for a@example.com")
        .with_risk(ApprovalRisk::Low)
        .with_acting(ActingContext::new(AgentPrincipal::derived("adk-rust", "finance")));
        let card_value = DecisionCard::from_request(&request, Some("https://home.example/ui/"));
        let drawn = card(&card_value, &heading(&card_value, None), FixedOffset::east_opt(0));
        let text = drawn["text"].as_str().unwrap();
        assert!(
            text.contains("Run &lt;!channel&gt; &amp; &lt;https://evil.example|click&gt;"),
            "{text}"
        );
        assert!(!drawn.to_string().contains("a@example.com"));
        let elements = &drawn["blocks"].as_array().unwrap().last().unwrap()["elements"];
        let labels: Vec<&str> = elements
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["text"]["text"].as_str().unwrap())
            .collect();
        assert_eq!(labels, ["Allow once", "Deny", "Open in home"]);
        let press = Press { card: None, blocks: drawn["blocks"].clone(), text: text.into() };
        let after = answered(&press);
        let elements = &after["blocks"].as_array().unwrap().last().unwrap()["elements"];
        assert_eq!(elements.as_array().unwrap().len(), 1, "only the home link stays");
        assert_eq!(plain("<@U2> *hi*")[0]["text"], "&lt;@U2&gt; *hi*");
        assert_eq!(plain("<@U2> *hi*")[0]["mrkdwn"], false);
    }
}
