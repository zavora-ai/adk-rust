//! What crosses the wire to and from Telegram (spec 3.3 §5.3, §8, S-I4, S-I5, S-I6, S-I9):
//! updates read into the kernel's inbound values, and the kernel's cards and notices drawn
//! as Telegram messages with buttons.

use adk_kernel::channels::{
    ChannelAnswer, ChannelInbound, ChannelNotice, DecisionCard, InboundBody, StopNotice,
};
use chrono::{DateTime, FixedOffset, Utc};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::TELEGRAM_CHANNEL;
use crate::project::humanize;
use crate::rules::Risk;
use crate::words;

/// Telegram's limit on a button's callback data, in bytes.
const MAX_CALLBACK_BYTES: usize = 64;
/// Telegram's limit on a message's text, in UTF-16 code units.
pub(crate) const MAX_MESSAGE_UNITS: usize = 4096;
/// The longest text taken from a person, in characters: Telegram's own limit.
const MAX_INBOUND_CHARS: usize = 4096;

// ---------------------------------------------------------------------------
// In: updates
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Update {
    #[serde(default)]
    message: Option<Message>,
    #[serde(default)]
    callback_query: Option<CallbackQuery>,
}

#[derive(Deserialize)]
struct Message {
    message_id: i64,
    #[serde(default)]
    from: Option<User>,
    chat: Chat,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct User {
    #[serde(deserialize_with = "id")]
    id: String,
    #[serde(default)]
    is_bot: bool,
    #[serde(default)]
    first_name: String,
    #[serde(default)]
    last_name: Option<String>,
}

#[derive(Deserialize)]
struct Chat {
    #[serde(deserialize_with = "id")]
    id: String,
    #[serde(rename = "type", default)]
    kind: String,
}

/// A Telegram id as text: the Bot API sends numbers; text is read too, as the `@name` form
/// of a chat id, and as the names the kernel's contract suite gives its accounts.
fn id<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Id {
        Number(i64),
        Text(String),
    }
    Ok(match Id::deserialize(deserializer)? {
        Id::Number(number) => number.to_string(),
        Id::Text(text) => text,
    })
}

#[derive(Deserialize)]
struct CallbackQuery {
    id: String,
    from: User,
    #[serde(default)]
    message: Option<Message>,
    #[serde(default)]
    data: Option<String>,
}

/// A pressed button, which Telegram waits to have acknowledged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Press {
    /// The callback query's id.
    pub(crate) callback_id: String,
    /// The chat and message the button was on.
    pub(crate) card: Option<(String, i64)>,
}

/// One update as the adapter reads it: what it gives the gateway, and the button to
/// acknowledge.
#[derive(Debug, Default)]
pub(crate) struct Parsed {
    pub(crate) inbound: Option<ChannelInbound>,
    pub(crate) press: Option<Press>,
}

/// Reads one update (S-I6). A new text message in a private chat from a person becomes a
/// typed message; a pressed answer button in a private chat becomes an answer; everything
/// else — groups and channels, bots, edits, media, other update kinds — is ignored, so a card
/// never reaches a thread other people read and only the account itself speaks for it.
pub(crate) fn parse(update: &Value) -> Parsed {
    let Ok(update) = Update::deserialize(update) else {
        return Parsed::default();
    };
    if let Some(message) = update.message {
        return Parsed { inbound: typed(message), press: None };
    }
    if let Some(query) = update.callback_query {
        let card =
            query.message.as_ref().map(|message| (message.chat.id.clone(), message.message_id));
        let inbound = pressed(&query);
        return Parsed { inbound, press: Some(Press { callback_id: query.id, card }) };
    }
    Parsed::default()
}

fn typed(message: Message) -> Option<ChannelInbound> {
    let from = message.from.filter(|from| !from.is_bot)?;
    if message.chat.kind != "private" || from.id.is_empty() || message.chat.id.is_empty() {
        return None;
    }
    let text = clean_inbound(message.text.as_deref()?)?;
    let inbound = ChannelInbound::text(
        TELEGRAM_CHANNEL,
        from.id.clone(),
        message.chat.id,
        message.message_id.to_string(),
        text,
    );
    Some(inbound.with_display_name(display_name(&from)))
}

/// A pressed answer: only a button's data answers a decision, never typed text (S-I4).
fn pressed(query: &CallbackQuery) -> Option<ChannelInbound> {
    if query.from.is_bot || query.from.id.is_empty() {
        return None;
    }
    let message = query
        .message
        .as_ref()
        .filter(|message| message.chat.kind == "private" && !message.chat.id.is_empty())?;
    let (answer, version, decision) = parse_callback(query.data.as_deref()?)?;
    let mut inbound = ChannelInbound::text(
        TELEGRAM_CHANNEL,
        query.from.id.clone(),
        message.chat.id.clone(),
        format!("cb-{}", query.id),
        String::new(),
    )
    .with_display_name(display_name(&query.from));
    inbound.body = InboundBody::Answer { decision, version, answer };
    Some(inbound)
}

fn display_name(user: &User) -> String {
    match &user.last_name {
        Some(last) => format!("{} {last}", user.first_name),
        None => user.first_name.clone(),
    }
}

/// A person's text as the gateway gets it: hidden characters dropped, at most Telegram's
/// own length, and `/start` without a deep link's payload, since a link anyone can share
/// would otherwise put words in the person's mouth (S-I6). `None` when nothing is left.
fn clean_inbound(text: &str) -> Option<String> {
    let text = start_command(text).unwrap_or(text);
    let text: String = text.chars().filter(|c| !is_hidden(*c)).take(MAX_INBOUND_CHARS).collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// `"/start"` when `text` is the `/start` command, with or without a payload or the bot's
/// name.
fn start_command(text: &str) -> Option<&'static str> {
    let command = text.split_whitespace().next()?;
    let name = command.split('@').next().unwrap_or_default();
    name.eq_ignore_ascii_case("/start").then_some("/start")
}

/// Control characters other than newline and tab, and the bidirectional controls that
/// reorder what a reader sees.
pub(crate) fn is_hidden(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t')
        || matches!(
            c,
            '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
        )
}

/// A button's data for `answer` on `decision` at `version`: `y:` (allow once) or `n:`
/// (deny), the version, and the decision. `None` when it does not fit Telegram's 64 bytes.
pub(crate) fn callback_data(answer: ChannelAnswer, decision: &str, version: u64) -> Option<String> {
    let tag = match answer {
        ChannelAnswer::AllowOnce => 'y',
        ChannelAnswer::Deny => 'n',
        // An answer this adapter does not know gets no button.
        _ => return None,
    };
    let data = format!("{tag}:{version}:{decision}");
    (!decision.is_empty() && data.len() <= MAX_CALLBACK_BYTES).then_some(data)
}

/// The answer, version and decision a button's `data` carries; `None` for anything else.
pub(crate) fn parse_callback(data: &str) -> Option<(ChannelAnswer, u64, String)> {
    if data.len() > MAX_CALLBACK_BYTES {
        return None;
    }
    let mut parts = data.splitn(3, ':');
    let answer = match parts.next()? {
        "y" => ChannelAnswer::AllowOnce,
        "n" => ChannelAnswer::Deny,
        _ => return None,
    };
    let version = parts.next()?;
    if version.is_empty() || !version.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let version = version.parse().ok()?;
    let decision = parts.next()?;
    if decision.is_empty() || decision.chars().any(char::is_control) {
        return None;
    }
    Some((answer, version, decision.to_string()))
}

// ---------------------------------------------------------------------------
// Out: messages
// ---------------------------------------------------------------------------

/// A message for `sendMessage`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Outgoing {
    pub(crate) text: String,
    /// The text is Telegram HTML.
    pub(crate) html: bool,
    pub(crate) keyboard: Option<Value>,
}

impl Outgoing {
    pub(crate) fn plain(text: String) -> Self {
        Self { text, html: false, keyboard: None }
    }

    fn html(text: String) -> Self {
        Self { text, html: true, keyboard: None }
    }
}

/// A decision card as the chat screen of spec 3.3 §5.3 draws it: "Waiting for you · since
/// 18:00", the summary, "Finance · Medium risk", a button for each answer the card offers and
/// no others, and "Open in home". Nothing of the call's arguments is added to what the
/// kernel's card carries (S-I5); the decision's id and version ride in the buttons' data,
/// never in the text (§8).
pub(crate) fn card(card: &DecisionCard, offset: FixedOffset) -> Outgoing {
    let buttons: Option<Vec<Value>> = card
        .answers
        .iter()
        .map(|answer| {
            callback_data(*answer, &card.decision, card.version)
                .map(|data| json!({ "text": answer.label(), "callback_data": data }))
        })
        .collect();
    let buttons = buttons.unwrap_or_default();
    let mut lines = vec![format!(
        "{} · {}",
        words::WAITING_FOR_YOU,
        words::since(&hh_mm(card.asked_at, offset))
    )];
    if !card.summary.trim().is_empty() {
        lines.push(format!("<b>{}</b>", html(&card.summary)));
    }
    lines.push(format!("{} · {}", html(&humanize(&card.agent)), Risk::from(card.risk).words()));
    if card.answers.is_empty() {
        lines.push(words::NEEDS_A_SCREEN.to_string());
    } else if buttons.is_empty() {
        // The decision's id is too long for a button: it is answered in the home.
        lines.push(html(&ChannelNotice::DecideInHome { home: None }.text()));
    }
    if let Some(link) = home_link(card.home.as_deref()) {
        lines.push(link);
    }
    let mut message = Outgoing::html(lines.join("\n"));
    if !buttons.is_empty() {
        message.keyboard = Some(json!({ "inline_keyboard": [buttons] }));
    }
    message
}

/// "All agents stopped. Use Resume all in the home app." (spec 3.3 §5.3, S-I10).
pub(crate) fn stopped(notice: &StopNotice) -> Outgoing {
    with_home(words::ALL_AGENTS_STOPPED_RESUME_IN_HOME, notice.home.as_deref())
}

/// A notice in the words of the chat screen (spec 3.3 §5.3, §8). `replying` is set when the
/// message answers the card it is about, so a decided line need not repeat its summary.
pub(crate) fn notice(
    notice: &ChannelNotice,
    offset: FixedOffset,
    now: DateTime<Utc>,
    replying: bool,
) -> Outgoing {
    match notice {
        ChannelNotice::PairingCode { code, expires_at } => Outgoing::html(format!(
            "This account isn't paired yet. To pair it, run <code>adk-rust pair accept {}</code> \
             where your agents run, before {}.",
            html(code),
            hh_mm(*expires_at, offset)
        )),
        ChannelNotice::Decided { answer, summary } => {
            let line = match answer {
                ChannelAnswer::AllowOnce => format!("You allowed it once · {}", hh_mm(now, offset)),
                ChannelAnswer::Deny => format!("You denied it · {}", hh_mm(now, offset)),
                _ => return Outgoing::plain(clean_outbound(&notice.text())),
            };
            if replying {
                Outgoing::plain(line)
            } else {
                Outgoing::plain(clean_outbound(&format!("{line}\n{summary}")))
            }
        }
        ChannelNotice::Stopped { home } => {
            with_home(words::ALL_AGENTS_STOPPED_RESUME_IN_HOME, home.as_deref())
        }
        ChannelNotice::DecideInHome { home } => {
            with_home(&ChannelNotice::DecideInHome { home: None }.text(), home.as_deref())
        }
        ChannelNotice::ResumeRefused { home } => {
            with_home(&ChannelNotice::ResumeRefused { home: None }.text(), home.as_deref())
        }
        other => Outgoing::plain(clean_outbound(&other.text())),
    }
}

/// `sentence`, then "Open in home" linking to `home` when it is a web address.
fn with_home(sentence: &str, home: Option<&str>) -> Outgoing {
    let mut text = html(sentence);
    if let Some(link) = home_link(home) {
        text.push('\n');
        text.push_str(&link);
    }
    Outgoing::html(text)
}

/// "Open in home" as a link, for an `http` or `https` address only.
fn home_link(home: Option<&str>) -> Option<String> {
    let home = home?;
    let web = home.starts_with("https://") || home.starts_with("http://");
    web.then(|| format!("<a href=\"{}\">{}</a>", html(home), words::OPEN_IN_HOME))
}

/// An agent's text as Telegram shows it: plain text, never parsed as markup, without hidden
/// characters (S-I9), in messages within Telegram's length. Empty text sends nothing.
pub(crate) fn plain_chunks(text: &str) -> Vec<String> {
    split(&clean_outbound(text), MAX_MESSAGE_UNITS)
        .into_iter()
        .filter(|chunk| !chunk.trim().is_empty())
        .collect()
}

/// `text` without hidden characters, trimmed.
fn clean_outbound(text: &str) -> String {
    text.chars().filter(|c| !is_hidden(*c)).collect::<String>().trim().to_string()
}

/// `text` in pieces of at most `max_units` UTF-16 code units, each ending after a newline
/// in its second half when there is one. The pieces put together are `text`.
pub(crate) fn split(text: &str, max_units: usize) -> Vec<String> {
    let max_units = max_units.max(2);
    let mut pieces = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut units = 0;
        let mut cut = rest.len();
        for (index, c) in rest.char_indices() {
            units += c.len_utf16();
            if units > max_units {
                cut = index;
                break;
            }
        }
        if cut < rest.len()
            && let Some(newline) = rest[..cut].rfind('\n').filter(|newline| *newline >= cut / 2)
        {
            cut = newline + 1;
        }
        pieces.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
    pieces
}

/// Text for Telegram HTML: hidden characters dropped, and `&`, `<`, `>` and `"` escaped
/// (S-I9).
pub(crate) fn html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().filter(|c| !is_hidden(*c)) {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// `at` as `HH:MM` at `offset`.
pub(crate) fn hh_mm(at: DateTime<Utc>, offset: FixedOffset) -> String {
    at.with_timezone(&offset).format("%H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use adk_core::{
        ActingContext, AgentPrincipal, ApprovalPolicy, ApprovalRequest, ApprovalRisk,
        ApprovalScope, ApproverSelector, ToolConfirmationRequest,
    };
    use proptest::prelude::*;

    use super::*;

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).unwrap()
    }

    fn message(chat_type: &str, from_bot: bool, text: &str) -> Value {
        json!({
            "update_id": 7,
            "message": {
                "message_id": 31,
                "from": { "id": 4217, "is_bot": from_bot, "first_name": "Ana", "last_name": "M" },
                "chat": { "id": 4217, "type": chat_type },
                "date": 0,
                "text": text,
            }
        })
    }

    fn press(chat_type: &str, data: &str) -> Value {
        json!({
            "update_id": 8,
            "callback_query": {
                "id": "cb-9",
                "from": { "id": 4217, "is_bot": false, "first_name": "Ana" },
                "message": { "message_id": 40, "chat": { "id": 4217, "type": chat_type }, "date": 0 },
                "data": data,
            }
        })
    }

    fn request(summary: &str, args: Value, risk: ApprovalRisk) -> ApprovalRequest {
        let call = ToolConfirmationRequest::new("send_email", args);
        ApprovalRequest::new(
            "apr_0123456789abcdef0123456789abcdef",
            ApprovalScope::new("adk-rust", "ana", "s1"),
            &call,
            ApprovalPolicy::new([ApproverSelector::group("operator")]),
        )
        .with_summary(summary)
        .with_risk(risk)
        .with_acting(ActingContext::new(AgentPrincipal::derived("adk-rust", "finance")))
    }

    #[test]
    fn a_private_message_is_a_typed_message() {
        let parsed = parse(&message("private", false, "  what's on today?  "));
        let inbound = parsed.inbound.unwrap();
        assert_eq!(
            inbound,
            ChannelInbound::text(TELEGRAM_CHANNEL, "4217", "4217", "31", "what's on today?")
                .with_display_name("Ana M")
        );
        assert_eq!(parsed.press, None);
        // An id given as text is read as it is; an empty one is no one.
        let mut named = message("private", false, "hello");
        named["message"]["from"]["id"] = json!("ana-chat");
        named["message"]["chat"]["id"] = json!("t-ana");
        let inbound = parse(&named).inbound.unwrap();
        assert_eq!((inbound.account.as_str(), inbound.thread.as_str()), ("ana-chat", "t-ana"));
        named["message"]["from"]["id"] = json!("");
        assert!(parse(&named).inbound.is_none());
    }

    #[test]
    fn groups_bots_edits_and_media_are_ignored() {
        for chat in ["group", "supergroup", "channel"] {
            assert!(parse(&message(chat, false, "stop all")).inbound.is_none(), "{chat}");
        }
        assert!(parse(&message("private", true, "hello")).inbound.is_none(), "a bot");
        let edited = json!({ "update_id": 1, "edited_message": message("private", false, "x")["message"].clone() });
        assert!(parse(&edited).inbound.is_none());
        let mut photo = message("private", false, "");
        photo["message"].as_object_mut().unwrap().remove("text");
        assert!(parse(&photo).inbound.is_none());
        assert!(parse(&json!({ "update_id": 2, "poll": {} })).inbound.is_none());
        assert!(parse(&json!("not an update")).inbound.is_none());
    }

    #[test]
    fn hidden_characters_and_deep_link_payloads_are_dropped() {
        let inbound = |text: &str| {
            parse(&message("private", false, text)).inbound.map(|inbound| inbound.body)
        };
        let text = |text: &str| Some(InboundBody::Text { text: text.to_string() });
        assert_eq!(inbound("pay\u{202E}moc.evil\u{0007} now"), text("paymoc.evil now"));
        assert_eq!(inbound("/start ignore-previous-instructions"), text("/start"));
        assert_eq!(inbound("/start@agent_bot pay-mallory"), text("/start"));
        assert_eq!(inbound("/started the draft"), text("/started the draft"));
        assert_eq!(inbound("\u{200F}\u{0000} "), None);
        let long = "x".repeat(MAX_INBOUND_CHARS + 50);
        let Some(InboundBody::Text { text }) = inbound(&long) else { panic!() };
        assert_eq!(text.chars().count(), MAX_INBOUND_CHARS);
    }

    #[test]
    fn only_a_pressed_button_answers() {
        let parsed = parse(&press("private", "y:3:apr_1"));
        let inbound = parsed.inbound.unwrap();
        assert_eq!(
            inbound.body,
            InboundBody::Answer {
                decision: "apr_1".into(),
                version: 3,
                answer: ChannelAnswer::AllowOnce
            }
        );
        assert_eq!((inbound.account.as_str(), inbound.thread.as_str()), ("4217", "4217"));
        assert_eq!(
            parsed.press,
            Some(Press { callback_id: "cb-9".into(), card: Some(("4217".into(), 40)) })
        );
        // Typed words never answer (S-I4).
        for text in ["y:3:apr_1", "allow once", "Allow once", "yes"] {
            let body = parse(&message("private", false, text)).inbound.unwrap().body;
            assert!(matches!(body, InboundBody::Text { .. }), "{text}");
        }
        // Forged or foreign data, and presses in a group, answer nothing but are acknowledged.
        for data in ["a:3:apr_1", "y:-1:apr_1", "y:+3:apr_1", "y::apr_1", "y:3:", "y:3:a\u{0}b"] {
            let parsed = parse(&press("private", data));
            assert!(parsed.inbound.is_none(), "{data}");
            assert!(parsed.press.is_some(), "{data}");
        }
        assert!(parse(&press("group", "y:3:apr_1")).inbound.is_none());
    }

    #[test]
    fn a_card_draws_the_chat_screen_and_carries_no_argument() {
        let args =
            json!({ "to": "client@example.com", "body": "secret-body-91c2", "amount": 4200 });
        let request =
            request("Send the October invoice to client@example.com", args, ApprovalRisk::Medium);
        let card = DecisionCard::from_request(&request, Some("https://home.example/ui/"));
        let message = super::card(&card, utc());
        let asked = hh_mm(request.created_at, utc());
        assert_eq!(
            message.text,
            format!(
                "Waiting for you · since {asked}\n<b>Send the October invoice to […]</b>\n\
                 Finance · Medium risk\n<a href=\"https://home.example/ui/\">Open in home</a>"
            )
        );
        assert!(message.html);
        let keyboard = message.keyboard.unwrap();
        assert_eq!(
            keyboard,
            json!({ "inline_keyboard": [[
                { "text": "Allow once", "callback_data": format!("y:{}:{}", card.version, card.decision) },
                { "text": "Deny", "callback_data": format!("n:{}:{}", card.version, card.decision) },
            ]] })
        );
        let wire = serde_json::to_string(&json!([message.text, keyboard])).unwrap();
        for argument in ["client@example.com", "secret-body-91c2", "4200"] {
            assert!(!wire.contains(argument), "{argument} left the machine: {wire}");
        }
    }

    #[test]
    fn a_card_chat_may_not_decide_links_to_the_home_without_buttons() {
        for risk in [ApprovalRisk::High, ApprovalRisk::Critical] {
            let request = request("Publish the release", json!({}), risk);
            let card = DecisionCard::from_request(&request, Some("http://127.0.0.1:8080/ui/"));
            let message = super::card(&card, utc());
            assert!(message.keyboard.is_none());
            assert!(message.text.contains("High risk"), "{}", message.text);
            assert!(message.text.contains(words::NEEDS_A_SCREEN));
            assert!(
                message.text.ends_with("<a href=\"http://127.0.0.1:8080/ui/\">Open in home</a>")
            );
        }
        // A decision whose id does not fit a button is answered in the home.
        let mut request = request("Send it", json!({}), ApprovalRisk::Low);
        request.id = "d".repeat(70);
        let message = super::card(&DecisionCard::from_request(&request, None), utc());
        assert!(message.keyboard.is_none());
        assert!(message.text.contains("Decide this one in the home."), "{}", message.text);
        // Only http(s) homes are linked.
        let card = DecisionCard::from_request(&request, Some("javascript:alert(1)"));
        assert!(!super::card(&card, utc()).text.contains("href"));
    }

    #[test]
    fn text_from_agents_and_tools_is_escaped() {
        let request = request("Run <b>rm</b> & \"x\"\u{202E}", json!({}), ApprovalRisk::Low);
        let message = super::card(&DecisionCard::from_request(&request, None), utc());
        assert!(
            message.text.contains("<b>Run &lt;b&gt;rm&lt;/b&gt; &amp; &quot;x&quot;</b>"),
            "{}",
            message.text
        );
        assert_eq!(plain_chunks("  Done.\u{202E}\u{0007}\n  "), ["Done."]);
        assert!(plain_chunks(" \u{200E} ").is_empty());
    }

    #[test]
    fn notices_use_the_chat_screens_words() {
        let at = DateTime::parse_from_rfc3339("2026-10-24T07:31:00Z").unwrap().to_utc();
        let decided = ChannelNotice::Decided {
            answer: ChannelAnswer::AllowOnce,
            summary: "Send the invoice".into(),
        };
        assert_eq!(notice(&decided, utc(), at, true).text, "You allowed it once · 07:31");
        assert_eq!(
            notice(&decided, utc(), at, false).text,
            "You allowed it once · 07:31\nSend the invoice"
        );
        let denied =
            ChannelNotice::Decided { answer: ChannelAnswer::Deny, summary: "Send it".into() };
        assert_eq!(notice(&denied, utc(), at, true).text, "You denied it · 07:31");
        let stopped: StopNotice =
            serde_json::from_value(json!({ "at": at, "home": "https://home.example" })).unwrap();
        assert_eq!(
            super::stopped(&stopped).text,
            "All agents stopped. Use Resume all in the home app.\n\
             <a href=\"https://home.example\">Open in home</a>"
        );
        let code = ChannelNotice::PairingCode { code: "ABCD-2345".into(), expires_at: at };
        let message = notice(&code, utc(), at, false);
        assert!(message.html);
        assert!(message.text.contains("<code>adk-rust pair accept ABCD-2345</code>"));
        assert!(message.text.ends_with("before 07:31."), "{}", message.text);
        let resume = ChannelNotice::ResumeRefused { home: None };
        assert_eq!(notice(&resume, utc(), at, false).text, "Resuming happens only in the home.");
        let gone = ChannelNotice::DecisionGone;
        assert_eq!(notice(&gone, utc(), at, false), Outgoing::plain(gone.text()));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Pieces stay within the limit and put together give the text back: nothing lost,
        /// nothing repeated.
        #[test]
        fn split_has_no_gap_and_no_overlap(
            text in "(\\PC|\n|😀){0,600}",
            max in 2usize..80,
        ) {
            let pieces = split(&text, max);
            prop_assert_eq!(pieces.concat(), text.clone());
            for piece in &pieces {
                prop_assert!(!piece.is_empty());
                prop_assert!(piece.encode_utf16().count() <= max);
            }
        }

        /// What a button carries is read back as it was written, within 64 bytes.
        #[test]
        fn callback_data_round_trips(
            allow in any::<bool>(),
            version in any::<u64>(),
            decision in "[a-zA-Z0-9_:.-]{1,60}",
        ) {
            let answer = if allow { ChannelAnswer::AllowOnce } else { ChannelAnswer::Deny };
            match callback_data(answer, &decision, version) {
                Some(data) => {
                    prop_assert!(data.len() <= MAX_CALLBACK_BYTES);
                    prop_assert_eq!(parse_callback(&data), Some((answer, version, decision)));
                }
                None => prop_assert!(decision.len() + 3 + version.to_string().len() > MAX_CALLBACK_BYTES),
            }
        }

        /// Escaped text has no markup of its own and reads back as the text.
        #[test]
        fn html_escapes_every_markup_character(text in "\\PC{0,200}") {
            let escaped = html(&text);
            prop_assert!(!escaped.contains('<') && !escaped.contains('>') && !escaped.contains('"'));
            let back = escaped
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&amp;", "&");
            let visible: String = text.chars().filter(|c| !is_hidden(*c)).collect();
            prop_assert_eq!(back, visible);
        }
    }
}
