//! What crosses the wire to and from Microsoft Teams (spec 3.3 S-I4, S-I5, S-I6, S-I9): Bot
//! Framework activities read into the kernel's inbound values, and the kernel's cards and
//! notices drawn as plain-text messages and Adaptive Cards.

use adk_kernel::channels::{ChannelInbound, DecisionCard, InboundBody};
use chrono::FixedOffset;
use serde_json::{Value, json};

use super::TEAMS_CHANNEL;
use crate::channels::shared::{
    Heading, Piece, Words, buttons, card_words, chunks, clean_inbound, parse_callback, truncate,
};

/// The Bot Framework channel id of Microsoft Teams.
pub(crate) const MSTEAMS: &str = "msteams";
/// The invoke an `Action.Execute` sends.
pub(crate) const CARD_ACTION: &str = "adaptiveCard/action";
/// The verb of a card's answer buttons.
pub(crate) const ANSWER_VERB: &str = "adk.answer";
/// The key of a button's data that carries the answer, `y:` or `n:`, the version and the
/// decision.
pub(crate) const ANSWER_KEY: &str = "adk";
/// The content type of an Adaptive Card attachment.
pub(crate) const ADAPTIVE_CARD: &str = "application/vnd.microsoft.card.adaptive";
/// The Adaptive Card version: 1.4 introduces `Action.Execute`; older clients take the
/// `Action.Submit` fallback.
const CARD_VERSION: &str = "1.4";
/// Characters per message: Teams takes about 80 KB of UTF-16 safely.
const MAX_TEXT_CHARS: usize = 16_000;
/// The longest notification preview of a card.
const MAX_SUMMARY_CHARS: usize = 120;

/// What one verified activity carries.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Received {
    /// The activity's id, by which a redelivery is known.
    pub(crate) id: String,
    /// The conversation: replies and cards go to it.
    pub(crate) thread: String,
    /// The Bot Connector address of the conversation.
    pub(crate) service_url: String,
    /// The Microsoft Entra tenant the conversation is in.
    pub(crate) tenant: Option<String>,
    /// What the gateway is given, when anything.
    pub(crate) inbound: Option<ChannelInbound>,
    /// The card a pressed button was on.
    pub(crate) card: Option<String>,
    /// Whether it is an invoke, answered in the HTTP response.
    pub(crate) invoke: bool,
}

fn text_of(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// Reads one activity, whose token was checked (S-I6). A message in a one-on-one chat becomes
/// a typed message; a pressed answer — an `Action.Execute` invoke, or the `Action.Submit` an
/// older client sends in its place — becomes an answer. `None` for another channel, a group
/// chat or a channel conversation, and anything without an id, a sender or a conversation;
/// other activities (installs, typing, receipts) come back with no inbound value so their
/// service address is still learned.
pub(crate) fn parse(activity: &Value) -> Option<Received> {
    if activity["channelId"] != MSTEAMS {
        return None;
    }
    let conversation = &activity["conversation"];
    // One-on-one chats only: in a group, anyone in it could answer another's card.
    if conversation["conversationType"].as_str().is_some_and(|kind| kind != "personal")
        || conversation["isGroup"] == true
    {
        return None;
    }
    let (id, from, thread) =
        (text_of(&activity["id"]), text_of(&activity["from"]["id"]), text_of(&conversation["id"]));
    let service_url = text_of(&activity["serviceUrl"]);
    if id.is_empty() || from.is_empty() || thread.is_empty() || service_url.is_empty() {
        return None;
    }
    let tenant = conversation["tenantId"]
        .as_str()
        .or_else(|| activity["channelData"]["tenant"]["id"].as_str())
        .map(str::to_string);
    let reply_to = activity["replyToId"].as_str().map(str::to_string);
    let answer = |data: &Value| {
        data[ANSWER_KEY].as_str().and_then(parse_callback).map(|(answer, version, decision)| {
            let mut inbound = ChannelInbound::text(TEAMS_CHANNEL, from, thread, id, "");
            inbound.body = InboundBody::Answer { decision, version, answer };
            inbound
        })
    };
    let (inbound, card, invoke) = match (activity["type"].as_str(), activity["name"].as_str()) {
        (Some("invoke"), Some(CARD_ACTION)) => {
            let action = &activity["value"]["action"];
            let inbound = (action["type"] == "Action.Execute" && action["verb"] == ANSWER_VERB)
                .then(|| answer(&action["data"]))
                .flatten();
            (inbound, reply_to, true)
        }
        (Some("invoke"), _) => (None, None, true),
        // An older client's `Action.Submit` sends the button's data as the message's value.
        (Some("message"), _) if activity["value"].is_object() => {
            (answer(&activity["value"]), reply_to, false)
        }
        (Some("message"), _) => {
            let inbound = clean_inbound(text_of(&activity["text"]))
                .map(|text| ChannelInbound::text(TEAMS_CHANNEL, from, thread, id, text));
            (inbound, None, false)
        }
        _ => (None, None, false),
    };
    let name = activity["from"]["name"].as_str().filter(|name| !name.is_empty());
    let inbound = match (inbound, name) {
        (Some(inbound), Some(name)) => Some(inbound.with_display_name(name)),
        (inbound, _) => inbound,
    };
    Some(Received {
        id: id.to_string(),
        thread: thread.to_string(),
        service_url: service_url.to_string(),
        tenant,
        inbound,
        card,
        invoke,
    })
}

/// A plain-text message: Teams shows `plain` text as written, so nothing an agent, a tool or
/// a person wrote formats or links by markup (S-I9).
pub(crate) fn text(body: &str) -> Value {
    json!({ "type": "message", "textFormat": "plain", "text": body })
}

/// An agent's text in messages within Teams' size. Empty text sends nothing.
pub(crate) fn plain(body: &str) -> Vec<Value> {
    chunks(body, MAX_TEXT_CHARS).iter().map(|chunk| text(chunk)).collect()
}

/// A notice in the chat screen's words, as plain text with "Open in home: url".
pub(crate) fn message(words: &Words) -> Value {
    text(&truncate(&words.text(), MAX_TEXT_CHARS))
}

/// `text` for a `TextRun`, which the Adaptive Cards schema renders without Markdown; a
/// Markdown link is still broken apart, since some Teams clients have parsed Markdown there.
fn run_text(text: &str) -> String {
    text.replace("](", "]\u{200B}(")
}

/// A line of the card as a `RichTextBlock` of `TextRun`s: a heading in bold, a command in
/// monospace.
fn rich_line(line: &[Piece], first: bool) -> Value {
    let inlines: Vec<Value> = line
        .iter()
        .filter_map(|piece| match piece {
            Piece::Plain(text) => Some(json!({ "type": "TextRun", "text": run_text(text) })),
            Piece::Bold(text) => {
                Some(json!({ "type": "TextRun", "text": run_text(text), "weight": "Bolder" }))
            }
            Piece::Code(text) => {
                Some(json!({ "type": "TextRun", "text": run_text(text), "fontType": "Monospace" }))
            }
            Piece::Link { .. } => None,
        })
        .collect();
    let mut block = json!({ "type": "RichTextBlock", "inlines": inlines });
    if first {
        block["inlines"][0]["isSubtle"] = json!(true);
    }
    block
}

/// A decision card (spec 3.3 §5.3) as an Adaptive Card: the chat screen's lines, an
/// `Action.Execute` for each answer the card offers and no others — falling back to
/// `Action.Submit` on older clients — and "Open in home" as an `Action.OpenUrl`. The
/// decision's id and version ride in each button's data, never in the text (§8), and nothing
/// of the call's arguments is added (S-I5).
pub(crate) fn card(card: &DecisionCard, heading: &Heading, offset: Option<FixedOffset>) -> Value {
    let words = card_words(card, heading, offset);
    let mut body: Vec<Value> = words
        .without_link()
        .lines
        .iter()
        .enumerate()
        .map(|(index, line)| rich_line(line, index == 0))
        .collect();
    let mut actions: Vec<Value> = buttons(card)
        .into_iter()
        .map(|button| {
            let data = json!({ ANSWER_KEY: button.data });
            json!({
                "type": "Action.Execute",
                "title": button.label,
                "verb": ANSWER_VERB,
                "data": data,
                "fallback": { "type": "Action.Submit", "title": button.label, "data": data },
            })
        })
        .collect();
    if let Some((url, label)) = words.link() {
        actions.push(json!({ "type": "Action.OpenUrl", "title": label, "url": url }));
    }
    if !actions.is_empty() {
        // Older clients honour `fallback` only inside an `ActionSet`.
        body.push(json!({ "type": "ActionSet", "actions": actions }));
    }
    let preview = format!("{}: {}", crate::words::WAITING_FOR_YOU, heading.title.trim());
    json!({
        "type": "message",
        "summary": truncate(&preview, MAX_SUMMARY_CHARS),
        "attachments": [{
            "contentType": ADAPTIVE_CARD,
            "content": {
                "type": "AdaptiveCard",
                "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                "version": CARD_VERSION,
                "body": body,
            },
        }],
    })
}

/// The body of the HTTP response to an `Action.Execute` invoke: the message Teams shows on
/// the card while the answer is handed over.
pub(crate) fn invoke_message(text: &str) -> Value {
    json!({ "statusCode": 200, "type": "application/vnd.microsoft.activity.message", "value": text })
}

/// The body of the HTTP response to an invoke the adapter does not take.
pub(crate) fn invoke_refused(message: &str) -> Value {
    json!({
        "statusCode": 400,
        "type": "application/vnd.microsoft.error",
        "value": { "code": "BadRequest", "message": message },
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

    const SERVICE: &str = "https://smba.trafficmanager.net/amer/";

    fn activity(kind: &str, extra: Value) -> Value {
        let mut activity = json!({
            "type": kind,
            "id": "1485983408511",
            "timestamp": "2026-11-18T09:10:07.437Z",
            "serviceUrl": SERVICE,
            "channelId": "msteams",
            "from": { "id": "29:ana", "name": "Ana", "aadObjectId": "7faf8ab2-3d56-4244-b585-20c8a42ed2b8" },
            "conversation": { "conversationType": "personal", "tenantId": "t1", "id": "a:dm-ana" },
            "recipient": { "id": "28:app", "name": "Agents" },
            "channelData": { "tenant": { "id": "t1" } },
        });
        if let (Some(target), Some(extra)) = (activity.as_object_mut(), extra.as_object()) {
            target.extend(extra.clone());
        }
        activity
    }

    fn decision_card(risk: ApprovalRisk) -> DecisionCard {
        let call =
            ToolConfirmationRequest::new("send_email", json!({ "to": "client@example.com" }));
        let request = ApprovalRequest::new(
            "apr_1",
            ApprovalScope::new("adk-rust", "ana", "s1"),
            &call,
            ApprovalPolicy::new([ApproverSelector::group("operator")]),
        )
        .with_summary("Send the invoice to client@example.com")
        .with_risk(risk)
        .with_acting(ActingContext::new(AgentPrincipal::derived("adk-rust", "finance")));
        DecisionCard::from_request(&request, Some("https://home.example/ui/"))
    }

    #[test]
    fn a_personal_message_is_typed_with_its_invisible_characters_dropped() {
        let received =
            parse(&activity("message", json!({ "text": " hi\u{200B} there " }))).unwrap();
        assert_eq!(
            received,
            Received {
                id: "1485983408511".into(),
                thread: "a:dm-ana".into(),
                service_url: SERVICE.into(),
                tenant: Some("t1".into()),
                inbound: Some(
                    ChannelInbound::text(
                        TEAMS_CHANNEL,
                        "29:ana",
                        "a:dm-ana",
                        "1485983408511",
                        "hi there"
                    )
                    .with_display_name("Ana")
                ),
                card: None,
                invoke: false,
            }
        );
    }

    #[test]
    fn only_a_pressed_button_answers_and_typed_data_never_does() {
        let data = json!({ "adk": "y:3:apr_1" });
        let invoke = activity(
            "invoke",
            json!({ "name": "adaptiveCard/action", "replyToId": "card-1",
                     "value": { "action": { "type": "Action.Execute", "verb": "adk.answer", "data": data }, "trigger": "manual" } }),
        );
        let received = parse(&invoke).unwrap();
        assert!(received.invoke);
        assert_eq!(received.card.as_deref(), Some("card-1"));
        assert_eq!(
            received.inbound.unwrap().body,
            InboundBody::Answer {
                decision: "apr_1".into(),
                version: 3,
                answer: ChannelAnswer::AllowOnce
            }
        );
        // The `Action.Submit` fallback.
        let submit =
            activity("message", json!({ "replyToId": "card-1", "value": { "adk": "n:3:apr_1" } }));
        assert_eq!(
            parse(&submit).unwrap().inbound.unwrap().body,
            InboundBody::Answer {
                decision: "apr_1".into(),
                version: 3,
                answer: ChannelAnswer::Deny
            }
        );
        // Typed text that looks like a button's data is only text (S-I4).
        let typed = parse(&activity("message", json!({ "text": "y:3:apr_1" }))).unwrap();
        assert!(matches!(typed.inbound.unwrap().body, InboundBody::Text { .. }));
        // Another verb, or no data, answers nothing; an invoke is still answered over HTTP.
        let other = activity(
            "invoke",
            json!({ "name": "adaptiveCard/action", "value": { "action": { "type": "Action.Execute", "verb": "other", "data": data } } }),
        );
        let other = parse(&other).unwrap();
        assert!(other.invoke && other.inbound.is_none());
    }

    #[test]
    fn group_chats_channels_and_other_bot_framework_channels_are_not_read() {
        let group = activity(
            "message",
            json!({ "text": "hi", "conversation": { "conversationType": "groupChat", "id": "19:g", "isGroup": true } }),
        );
        assert_eq!(parse(&group), None);
        let channel = activity(
            "message",
            json!({ "text": "hi", "conversation": { "conversationType": "channel", "id": "19:c" } }),
        );
        assert_eq!(parse(&channel), None);
        assert_eq!(
            parse(&activity("message", json!({ "text": "hi", "channelId": "webchat" }))),
            None
        );
        assert_eq!(parse(&activity("message", json!({ "text": "hi", "id": "" }))), None);
        // An install is read so its service address is learned, and gives the gateway nothing.
        let update = parse(&activity("conversationUpdate", json!({}))).unwrap();
        assert_eq!(update.inbound, None);
    }

    #[test]
    fn a_card_offers_its_answers_as_execute_actions_and_carries_no_argument() {
        let card = decision_card(ApprovalRisk::Medium);
        let drawn = super::card(&card, &heading(&card, None), FixedOffset::east_opt(0));
        let text = drawn.to_string();
        assert!(!text.contains("client@example.com"), "{text}");
        let content = &drawn["attachments"][0]["content"];
        assert_eq!(drawn["attachments"][0]["contentType"], ADAPTIVE_CARD);
        assert_eq!(content["version"], "1.4");
        let actions = content["body"].as_array().unwrap().last().unwrap()["actions"].clone();
        assert_eq!(
            actions,
            json!([
                { "type": "Action.Execute", "title": "Allow once", "verb": "adk.answer",
                  "data": { "adk": format!("y:{}:apr_1", card.version) },
                  "fallback": { "type": "Action.Submit", "title": "Allow once", "data": { "adk": format!("y:{}:apr_1", card.version) } } },
                { "type": "Action.Execute", "title": "Deny", "verb": "adk.answer",
                  "data": { "adk": format!("n:{}:apr_1", card.version) },
                  "fallback": { "type": "Action.Submit", "title": "Deny", "data": { "adk": format!("n:{}:apr_1", card.version) } } },
                { "type": "Action.OpenUrl", "title": "Open in home", "url": "https://home.example/ui/" },
            ])
        );
        assert!(drawn["summary"].as_str().unwrap().starts_with("Waiting for you: "));
        // High risk: no answer, only the home.
        let high = decision_card(ApprovalRisk::High);
        let drawn = super::card(&high, &heading(&high, None), None);
        let actions = drawn["attachments"][0]["content"]["body"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["actions"]
            .clone();
        assert_eq!(actions.as_array().unwrap().len(), 1);
        assert_eq!(actions[0]["type"], "Action.OpenUrl");
    }

    #[test]
    fn invoke_responses_follow_the_universal_action_model() {
        assert_eq!(
            invoke_message("Got it"),
            json!({ "statusCode": 200, "type": "application/vnd.microsoft.activity.message", "value": "Got it" })
        );
        assert_eq!(invoke_refused("no")["statusCode"], 400);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// Agent text goes out as plain text, nothing lost, in pieces within Teams' size.
        #[test]
        fn plain_text_is_sent_as_written(body in "[a-z*_\\[\\]()<> \n]{0,400}") {
            let sent: Vec<String> = plain(&body)
                .iter()
                .map(|message| {
                    prop_assert_eq!(&message["textFormat"], "plain");
                    Ok(message["text"].as_str().unwrap().to_string())
                })
                .collect::<Result<_, TestCaseError>>()?;
            let joined = sent.concat();
            prop_assert_eq!(joined.split_whitespace().collect::<Vec<_>>(),
                            body.split_whitespace().collect::<Vec<_>>());
        }

        /// No card text forms a Markdown link, whatever the summary says.
        #[test]
        fn card_text_never_forms_a_link(label in "[a-z]{1,8}", url in "[a-z]{1,8}") {
            let text = run_text(&format!("[{label}]({url})"));
            prop_assert!(!text.contains("]("));
        }
    }
}
