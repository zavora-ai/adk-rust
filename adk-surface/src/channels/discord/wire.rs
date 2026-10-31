//! What crosses the wire to and from Discord (spec 3.5 §10 X12; spec 3.3 S-I4, S-I5, S-I6,
//! S-I9): Gateway dispatches read into the kernel's inbound values, and the kernel's cards and
//! notices drawn as messages with button components.

use adk_kernel::channels::{ChannelInbound, DecisionCard, InboundBody};
use chrono::FixedOffset;
use serde_json::{Value, json};

use super::DISCORD_CHANNEL;
use crate::channels::media::FileRef;
use crate::channels::shared::{
    Heading, Piece, Words, buttons, card_words, chunks, clean_inbound, parse_callback, truncate,
};

/// Discord's limit on a message's content, in characters.
pub(crate) const MAX_CONTENT_CHARS: usize = 2000;
/// Discord's limit on a button's label, in characters.
const MAX_LABEL_CHARS: usize = 80;
/// The longest piece of an agent's reply in one message: room for its escapes.
const MAX_REPLY_CHARS: usize = 1000;
/// The application command that stops everything.
pub(crate) const STOP_COMMAND: &str = "stop";
/// The application command that asks for a pairing code (spec 3.5 §10 X12).
pub(crate) const PAIR_COMMAND: &str = "pair";

/// An interaction Discord waits to have answered within three seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Interaction {
    pub(crate) id: String,
    pub(crate) token: String,
    pub(crate) application: String,
    /// A command, answered later by editing its deferred reply; otherwise a button.
    pub(crate) command: bool,
}

/// A pressed button, and the card it was on.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Press {
    /// The card's channel and message.
    pub(crate) card: Option<(String, String)>,
    /// The card's components, to draw it again without its answers.
    pub(crate) components: Value,
}

/// One Gateway dispatch as the adapter reads it.
#[derive(Debug, Default)]
pub(crate) struct Parsed {
    pub(crate) inbound: Option<ChannelInbound>,
    pub(crate) press: Option<Press>,
    /// The files a message carried (spec 3.5 §10 X12).
    pub(crate) files: Vec<FileRef>,
    pub(crate) interaction: Option<Interaction>,
}

fn text_of(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// Reads one Gateway dispatch, `{op: 0, t, d}` (S-I6). A new message in a direct message
/// with the bot, from a person, becomes a typed message; a pressed answer button becomes an
/// answer; `/stop` stops everything; everything else — servers, bots, edits, other events —
/// is ignored, so a card never reaches a thread other people read. Every interaction is
/// returned to be answered, whatever it carries.
pub(crate) fn parse(payload: &Value) -> Parsed {
    let d = &payload["d"];
    match payload["t"].as_str() {
        Some("MESSAGE_CREATE") => match typed(d) {
            Some((inbound, files)) => Parsed { inbound: Some(inbound), files, ..Parsed::default() },
            None => Parsed::default(),
        },
        Some("INTERACTION_CREATE") => interaction(d),
        _ => Parsed::default(),
    }
}

fn display_name(user: &Value) -> Option<String> {
    user["global_name"]
        .as_str()
        .or_else(|| user["username"].as_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn typed(d: &Value) -> Option<(ChannelInbound, Vec<FileRef>)> {
    let author = &d["author"];
    // Type 0 is a message and 19 a reply; servers have a guild id.
    let kind = d["type"].as_u64().unwrap_or_default();
    if d.get("guild_id").is_some_and(|guild| !guild.is_null())
        || author["bot"].as_bool() == Some(true)
        || !matches!(kind, 0 | 19)
    {
        return None;
    }
    let (user, channel, id) =
        (text_of(&author["id"]), text_of(&d["channel_id"]), text_of(&d["id"]));
    if user.is_empty() || channel.is_empty() || id.is_empty() {
        return None;
    }
    let files: Vec<FileRef> = d["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|file| {
            Some(FileRef {
                url: file["url"].as_str()?.to_string(),
                media_type: file["content_type"].as_str().map(str::to_string),
                name: text_of(&file["filename"]).to_string(),
                size: file["size"].as_u64(),
            })
        })
        .collect();
    let text = match clean_inbound(text_of(&d["content"])) {
        Some(text) => text,
        None if !files.is_empty() => String::new(),
        None => return None,
    };
    let inbound = ChannelInbound::text(DISCORD_CHANNEL, user, channel, id, text);
    let inbound = match display_name(author) {
        Some(name) => inbound.with_display_name(name),
        None => inbound,
    };
    Some((inbound, files))
}

/// A component press or a command; only a pressed answer button answers a decision (S-I4).
fn interaction(d: &Value) -> Parsed {
    let kind = d["type"].as_u64().unwrap_or_default();
    let (id, token) = (text_of(&d["id"]), text_of(&d["token"]));
    if id.is_empty() || token.is_empty() || !matches!(kind, 2 | 3) {
        return Parsed::default();
    }
    let command = kind == 2;
    let interaction = Interaction {
        id: id.to_string(),
        token: token.to_string(),
        application: text_of(&d["application_id"]).to_string(),
        command,
    };
    let answered = Parsed { interaction: Some(interaction.clone()), ..Parsed::default() };
    // In a server the user is a member's; only direct messages are read.
    let user = &d["user"];
    let (account, channel) = (text_of(&user["id"]), text_of(&d["channel_id"]));
    if d.get("guild_id").is_some_and(|guild| !guild.is_null())
        || account.is_empty()
        || channel.is_empty()
    {
        return answered;
    }
    let message_id = format!("int-{id}");
    let (inbound, press) = if command {
        let inbound = match d["data"]["name"].as_str() {
            Some(STOP_COMMAND) => {
                ChannelInbound::stop_all(DISCORD_CHANNEL, account, channel, message_id)
            }
            Some(PAIR_COMMAND) => {
                ChannelInbound::text(DISCORD_CHANNEL, account, channel, message_id, "/pair")
            }
            _ => return answered,
        };
        (inbound, None)
    } else {
        let Some((answer, version, decision)) =
            d["data"]["custom_id"].as_str().and_then(parse_callback)
        else {
            return answered;
        };
        let message = &d["message"];
        let card = text_of(&message["id"]);
        let press = Press {
            card: (!card.is_empty()).then(|| (channel.to_string(), card.to_string())),
            components: message["components"].clone(),
        };
        let mut inbound = ChannelInbound::text(DISCORD_CHANNEL, account, channel, message_id, "");
        inbound.body = InboundBody::Answer { decision, version, answer };
        (inbound, Some(press))
    };
    let inbound = match display_name(user) {
        Some(name) => inbound.with_display_name(name),
        None => inbound,
    };
    Parsed { inbound: Some(inbound), press, interaction: Some(interaction), files: Vec::new() }
}

/// Text for Discord's markdown (S-I9): every character that formats, links, quotes, lists or
/// mentions is escaped with a backslash, which Discord shows as nothing.
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut line_start = true;
    let mut digits = false;
    for c in text.chars() {
        match c {
            '\\' | '*' | '_' | '~' | '`' | '|' | '[' | ']' | '<' | '>' | '@' => {
                out.push('\\');
                out.push(c);
            }
            // A heading, a list, a quote or a subtext at a line's start.
            '#' | '-' | '+' if line_start => {
                out.push('\\');
                out.push(c);
            }
            '.' if digits => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
        digits = (line_start || digits) && c.is_ascii_digit();
        line_start = c == '\n' || (line_start && c.is_whitespace());
    }
    out
}

/// No mention in what the bot sends pings anyone.
fn no_mentions() -> Value {
    json!({ "parse": [] })
}

/// A message's content from `words`: plain text escaped, a heading in bold, a pairing
/// command as code.
fn content(words: &Words) -> String {
    let text = words.without_link().render(|piece| match piece {
        Piece::Plain(text) => escape(text),
        Piece::Bold(text) => format!("**{}**", escape(text)),
        Piece::Code(text) => format!("`{}`", text.replace('`', "")),
        Piece::Link { .. } => String::new(),
    });
    truncate(&text, MAX_CONTENT_CHARS)
}

/// "Open in home" as a link button.
fn home_button(words: &Words) -> Option<Value> {
    let (url, label) = words.link()?;
    Some(json!({ "type": 2, "style": 5, "label": truncate(label, MAX_LABEL_CHARS), "url": url }))
}

/// A decision card (spec 3.3 §5.3): the chat screen's lines, a button for each answer the
/// card offers and no others, and "Open in home". The decision's id and version ride in the
/// buttons' custom ids, never in the text (§8).
pub(crate) fn card(card: &DecisionCard, heading: &Heading, offset: Option<FixedOffset>) -> Value {
    let words = card_words(card, heading, offset);
    let mut row: Vec<Value> = buttons(card)
        .into_iter()
        .map(|button| {
            json!({
                "type": 2,
                "style": 2,
                "label": truncate(button.label, MAX_LABEL_CHARS),
                "custom_id": button.data,
            })
        })
        .collect();
    row.extend(home_button(&words));
    let mut body =
        json!({ "content": content(&words), "allowed_mentions": no_mentions(), "flags": 4 });
    if !row.is_empty() {
        body["components"] = json!([{ "type": 1, "components": row }]);
    }
    body
}

/// A notice: escaped text, and "Open in home" as a link button.
pub(crate) fn message(words: &Words) -> Value {
    let mut body =
        json!({ "content": content(words), "allowed_mentions": no_mentions(), "flags": 4 });
    if let Some(button) = home_button(words) {
        body["components"] = json!([{ "type": 1, "components": [button] }]);
    }
    body
}

/// An agent's text as Discord shows it: escaped, never read as markdown and never pinging
/// anyone (S-I9), in messages within Discord's length. Empty text sends nothing.
pub(crate) fn plain(text: &str) -> Vec<Value> {
    chunks(text, MAX_REPLY_CHARS)
        .into_iter()
        .map(|chunk| {
            json!({ "content": truncate(&escape(&chunk), MAX_CONTENT_CHARS), "allowed_mentions": no_mentions(), "flags": 4 })
        })
        .collect()
}

/// The components of the card `press` was on, without its answer buttons, so it is not
/// pressed again; "Open in home" stays.
pub(crate) fn answered(press: &Press) -> Value {
    let rows: Vec<Value> = press
        .components
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let kept: Vec<Value> = row["components"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|component| component["style"] == 5)
                .cloned()
                .collect();
            (!kept.is_empty()).then(|| json!({ "type": 1, "components": kept }))
        })
        .collect();
    json!({ "components": rows })
}

/// `/stop` as an application command the bot declares, usable in its direct messages.
pub(crate) fn commands() -> Value {
    json!([
        {
            "name": STOP_COMMAND,
            "description": crate::words::STOP_ALL,
            "type": 1,
            "contexts": [1],
            "integration_types": [0, 1],
        },
        {
            "name": PAIR_COMMAND,
            "description": "Ask to pair this account",
            "type": 1,
            "contexts": [1],
            "integration_types": [0, 1],
        },
    ])
}

#[cfg(test)]
mod tests {
    use adk_kernel::channels::ChannelAnswer;
    use proptest::prelude::*;

    use super::*;

    fn message_create(content: &str) -> Value {
        json!({ "op": 0, "s": 3, "t": "MESSAGE_CREATE", "d": {
            "id": "m1", "channel_id": "c1", "type": 0, "content": content,
            "author": { "id": "u1", "username": "ana", "global_name": "Ana" },
        }})
    }

    #[test]
    fn a_direct_message_is_typed_and_everything_else_is_ignored() {
        let inbound = parse(&message_create(" hi\u{200B} there ")).inbound.unwrap();
        assert_eq!(
            inbound,
            ChannelInbound::text(DISCORD_CHANNEL, "u1", "c1", "m1", "hi there")
                .with_display_name("Ana")
        );
        let mut guild = message_create("stop all");
        guild["d"]["guild_id"] = json!("g1");
        assert!(parse(&guild).inbound.is_none());
        let mut bot = message_create("stop all");
        bot["d"]["author"]["bot"] = json!(true);
        assert!(parse(&bot).inbound.is_none());
        let mut pin = message_create("stop all");
        pin["d"]["type"] = json!(6);
        assert!(parse(&pin).inbound.is_none());
        let mut edit = message_create("stop all");
        edit["t"] = json!("MESSAGE_UPDATE");
        assert!(parse(&edit).inbound.is_none());
    }

    #[test]
    fn only_a_pressed_answer_button_answers_and_stop_is_a_command() {
        let press = |custom_id: &str| {
            json!({ "op": 0, "t": "INTERACTION_CREATE", "d": {
                "id": "i1", "token": "tok", "application_id": "a1", "type": 3, "channel_id": "c1",
                "user": { "id": "u1", "username": "ana" },
                "message": { "id": "m9", "components": [] },
                "data": { "custom_id": custom_id, "component_type": 2 },
            }})
        };
        let parsed = parse(&press("n:2:apr_1"));
        assert_eq!(
            parsed.inbound.unwrap().body,
            InboundBody::Answer {
                decision: "apr_1".into(),
                version: 2,
                answer: ChannelAnswer::Deny
            }
        );
        assert_eq!(parsed.press.unwrap().card, Some(("c1".into(), "m9".into())));
        assert!(!parsed.interaction.unwrap().command);
        let forged = parse(&press("q:2:apr_1"));
        assert!(forged.inbound.is_none() && forged.interaction.is_some(), "answered, not taken");
        let mut stop = press("");
        stop["d"]["type"] = json!(2);
        stop["d"]["data"] = json!({ "name": "stop" });
        let parsed = parse(&stop);
        assert_eq!(parsed.inbound.unwrap().body, InboundBody::StopAll);
        assert!(parsed.interaction.unwrap().command);
        stop["d"]["data"] = json!({ "name": "pair" });
        assert_eq!(parse(&stop).inbound.unwrap().body, InboundBody::Text { text: "/pair".into() });
        let mut guild = press("y:2:apr_1");
        guild["d"]["guild_id"] = json!("g1");
        assert!(parse(&guild).inbound.is_none());
    }

    #[test]
    fn markdown_and_mentions_are_escaped() {
        assert_eq!(
            escape("**bold** _i_ ~~s~~ `c` ||sp||"),
            "\\*\\*bold\\*\\* \\_i\\_ \\~\\~s\\~\\~ \\`c\\` \\|\\|sp\\|\\|"
        );
        assert_eq!(
            escape("[click](https://evil.example) <@123> @everyone"),
            "\\[click\\](https://evil.example) \\<\\@123\\> \\@everyone"
        );
        assert_eq!(escape("# h\n- item\n1. one\n> quote"), "\\# h\n\\- item\n1\\. one\n\\> quote");
        assert_eq!(escape("a-b 3.5"), "a-b 3.5");
        let body = &plain("hi @everyone")[0];
        assert_eq!(body["allowed_mentions"], json!({ "parse": [] }));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Escaped text has no markdown of its own and reads back as the text.
        #[test]
        fn escaping_reads_back(text in "\\PC{0,200}") {
            let escaped = escape(&text);
            let mut back = String::new();
            let mut chars = escaped.chars();
            while let Some(c) = chars.next() {
                if c == '\\' { back.extend(chars.next()); } else { back.push(c); }
            }
            prop_assert_eq!(back, text);
        }
    }
}
