//! Internet messages: reading inbound mail and writing outbound mail (RFC 5322, RFC 2045–2047,
//! RFC 3464, RFC 5965; spec 3.5 §7.13).

use base64::Engine as _;
use chrono::{DateTime, Utc};

use super::{InboundEmail, InboundKind, Mailbox, OutboundEmail};
use crate::error::{DriverError, Result};
use crate::outbound::bounded;

/// The longest inbound text kept, in characters.
pub(crate) const MAX_TEXT_CHARS: usize = 32_000;

/// A parsed entity: its headers, unfolded, and its raw body.
struct Entity<'a> {
    headers: Vec<(String, String)>,
    body: &'a [u8],
}

impl Entity<'_> {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The media type, lower case, and its parameters.
    fn content_type(&self) -> (String, Vec<(String, String)>) {
        let value = self.header("content-type").unwrap_or("text/plain; charset=us-ascii");
        let mut parts = split_params(value);
        let media = if parts.is_empty() {
            "text/plain".to_string()
        } else {
            parts.remove(0).trim().to_ascii_lowercase()
        };
        let params = parts
            .into_iter()
            .filter_map(|part| {
                let (key, value) = part.split_once('=')?;
                Some((key.trim().to_ascii_lowercase(), value.trim().trim_matches('"').to_string()))
            })
            .collect();
        (media, params)
    }
}

/// `value` split on `;` outside quotes.
fn split_params(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in value.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                current.push(c);
            }
            ';' if !quoted => out.push(std::mem::take(&mut current)),
            c => current.push(c),
        }
    }
    out.push(current);
    out
}

fn param<'a>(params: &'a [(String, String)], name: &str) -> Option<&'a str> {
    params.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
}

/// Splits `raw` into headers and body.
fn entity(raw: &[u8]) -> Entity<'_> {
    let (head, body) = match find(raw, b"\r\n\r\n") {
        Some(at) => (&raw[..at], &raw[at + 4..]),
        None => match find(raw, b"\n\n") {
            Some(at) => (&raw[..at], &raw[at + 2..]),
            None => (raw, &raw[raw.len()..]),
        },
    };
    let head = String::from_utf8_lossy(head);
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in head.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = headers.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    Entity { headers, body }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// The body of `entity` with its transfer encoding undone.
fn decoded_body(entity: &Entity<'_>) -> Vec<u8> {
    let encoding =
        entity.header("content-transfer-encoding").unwrap_or("7bit").trim().to_ascii_lowercase();
    match encoding.as_str() {
        "base64" => {
            let compact: Vec<u8> =
                entity.body.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
            base64::engine::general_purpose::STANDARD
                .decode(&compact)
                .unwrap_or_else(|_| entity.body.to_vec())
        }
        "quoted-printable" => quoted_printable_decode(entity.body),
        _ => entity.body.to_vec(),
    }
}

fn quoted_printable_decode(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut at = 0;
    while at < body.len() {
        match body[at] {
            b'=' if body.get(at + 1) == Some(&b'\r') && body.get(at + 2) == Some(&b'\n') => at += 3,
            b'=' if body.get(at + 1) == Some(&b'\n') => at += 2,
            b'=' => {
                let hex = body.get(at + 1..at + 3).and_then(|pair| std::str::from_utf8(pair).ok());
                match hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        at += 3;
                    }
                    None => {
                        out.push(b'=');
                        at += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                at += 1;
            }
        }
    }
    out
}

/// `bytes` as text in `charset`: UTF-8, US-ASCII and Latin-1 exactly, anything else as
/// lossy UTF-8.
fn text_in(bytes: &[u8], charset: Option<&str>) -> String {
    match charset.map(str::to_ascii_lowercase).as_deref() {
        Some("iso-8859-1" | "latin1" | "windows-1252" | "cp1252") => {
            bytes.iter().map(|b| char::from(*b)).collect()
        }
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// The text of an entity: the first `text/plain` part of a multipart, or its HTML with tags
/// taken out.
fn body_text(entity: &Entity<'_>, depth: usize) -> Option<String> {
    let (media, params) = entity.content_type();
    if depth > 8 {
        return None;
    }
    if media.starts_with("multipart/") {
        let boundary = param(&params, "boundary")?;
        let parts = split_multipart(entity.body, boundary);
        let entities: Vec<Entity<'_>> = parts.iter().map(|part| self::entity(part)).collect();
        let plain = entities.iter().find(|part| part.content_type().0 == "text/plain");
        if let Some(text) = plain.and_then(|part| body_text(part, depth + 1)) {
            return Some(text);
        }
        return entities.iter().find_map(|part| body_text(part, depth + 1));
    }
    let text = text_in(&decoded_body(entity), param(&params, "charset"));
    match media.as_str() {
        "text/plain" => Some(text),
        "text/html" => Some(strip_html(&text)),
        _ => None,
    }
}

/// The parts of a multipart body.
fn split_multipart<'a>(body: &'a [u8], boundary: &str) -> Vec<&'a [u8]> {
    let delimiter = format!("--{boundary}");
    let delimiter = delimiter.as_bytes();
    let mut parts = Vec::new();
    let mut starts: Vec<usize> = Vec::new();
    let mut at = 0;
    while let Some(found) = find(&body[at..], delimiter) {
        let position = at + found;
        let line_start = position == 0 || body[position - 1] == b'\n';
        if line_start {
            starts.push(position);
        }
        at = position + delimiter.len();
    }
    for pair in starts.windows(2) {
        let start = pair[0] + delimiter.len();
        let part = &body[start..pair[1]];
        let part = part.strip_prefix(b"\r\n").or_else(|| part.strip_prefix(b"\n")).unwrap_or(part);
        let part = part.strip_suffix(b"\r\n").or_else(|| part.strip_suffix(b"\n")).unwrap_or(part);
        parts.push(part);
    }
    parts
}

/// `html` as text: tags removed, block ends as line breaks, the common entities decoded.
pub(crate) fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut tag = String::new();
    for c in html.chars() {
        match (in_tag, c) {
            (false, '<') => {
                in_tag = true;
                tag.clear();
            }
            (true, '>') => {
                in_tag = false;
                let name = tag
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if matches!(name.as_str(), "br" | "br/" | "p" | "div" | "li" | "tr")
                    && !out.ends_with('\n')
                {
                    out.push('\n');
                }
            }
            (true, c) => tag.push(c),
            (false, c) => out.push(c),
        }
    }
    decode_entities(out.trim())
}

/// `text` with the named and numeric HTML entities decoded.
pub(crate) fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let end = after.find(';').filter(|end| *end <= 10);
        let decoded = end.and_then(|end| {
            let name = &after[1..end];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some(' '),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                    .and_then(char::from_u32),
            }?;
            Some((c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `value` with its RFC 2047 encoded words decoded.
pub(crate) fn decode_words(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    let mut last_was_word = false;
    while let Some(start) = rest.find("=?") {
        let between = &rest[..start];
        if !(last_was_word && between.trim().is_empty()) {
            out.push_str(between);
        }
        let after = &rest[start + 2..];
        let decoded = (|| {
            let (charset, after) = after.split_once('?')?;
            let (encoding, after) = after.split_once('?')?;
            let end = after.find("?=")?;
            let text = &after[..end];
            let bytes = match encoding.to_ascii_uppercase().as_str() {
                "B" => base64::engine::general_purpose::STANDARD.decode(text).ok()?,
                "Q" => quoted_printable_decode(text.replace('_', " ").as_bytes()),
                _ => return None,
            };
            let charset = charset.split('*').next().unwrap_or(charset);
            Some((text_in(&bytes, Some(charset)), &after[end + 2..]))
        })();
        match decoded {
            Some((text, remaining)) => {
                out.push_str(&text);
                rest = remaining;
                last_was_word = true;
            }
            None => {
                out.push_str("=?");
                rest = after;
                last_was_word = false;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The mailboxes of an address-list header.
pub(crate) fn mailboxes(value: &str) -> Vec<Mailbox> {
    let mut out = Vec::new();
    let mut current = String::new();
    let (mut quoted, mut angle) = (false, false);
    for c in value.chars() {
        match c {
            '"' => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            _ => {}
        }
        if c == ',' && !quoted && !angle {
            out.extend(mailbox(&current));
            current.clear();
        } else {
            current.push(c);
        }
    }
    out.extend(mailbox(&current));
    out
}

/// One mailbox: `Name <address>` or `address`.
pub(crate) fn mailbox(text: &str) -> Option<Mailbox> {
    let text = text.trim();
    let (name, address) = match (text.rfind('<'), text.rfind('>')) {
        (Some(open), Some(close)) if open < close => {
            let name = decode_words(text[..open].trim().trim_matches('"').trim());
            (Some(name).filter(|name| !name.is_empty()), text[open + 1..close].trim().to_string())
        }
        _ => (None, text.trim_matches(['<', '>']).to_string()),
    };
    let address = address.trim().to_string();
    (address.contains('@') && !address.contains(char::is_whitespace))
        .then_some(Mailbox { name, address })
}

/// The message ids of a header such as `References`, without angle brackets.
fn message_ids(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(|id| id.trim_matches(['<', '>', ',']).to_string())
        .filter(|id| !id.is_empty())
        .collect()
}

/// Reads the raw message `raw` that arrived at `received_at`, `fallback_id` naming it when it
/// carries no `Message-ID`.
pub(crate) fn parse_message(
    raw: &[u8],
    received_at: DateTime<Utc>,
    fallback_id: &str,
) -> InboundEmail {
    let top = entity(raw);
    let header = |name: &str| top.header(name).map(decode_words);
    let id = top
        .header("message-id")
        .map(message_ids)
        .and_then(|ids| ids.into_iter().next())
        .unwrap_or_else(|| fallback_id.to_string());
    let in_reply_to =
        top.header("in-reply-to").map(message_ids).and_then(|ids| ids.into_iter().next());
    let references = top.header("references").map(message_ids).unwrap_or_default();
    let thread =
        references.first().cloned().or_else(|| in_reply_to.clone()).unwrap_or_else(|| id.clone());
    let subject = header("subject").unwrap_or_default();
    let (media, params) = top.content_type();
    let mut kind = InboundKind::Message;
    let mut report_recipient = None;
    if media == "multipart/report" {
        let report_type = param(&params, "report-type").unwrap_or_default().to_ascii_lowercase();
        let parts = param(&params, "boundary")
            .map(|boundary| split_multipart(top.body, boundary))
            .unwrap_or_default();
        for part in parts {
            let part = entity(part);
            let part_type = part.content_type().0;
            let fields = String::from_utf8_lossy(&decoded_body(&part)).into_owned();
            if report_type == "delivery-status" && part_type == "message/delivery-status" {
                let failed = fields.lines().any(|line| {
                    line.split_once(':').is_some_and(|(name, value)| {
                        name.trim().eq_ignore_ascii_case("action")
                            && value.trim().eq_ignore_ascii_case("failed")
                    })
                });
                if failed {
                    kind = InboundKind::Bounce;
                    report_recipient =
                        report_field(&fields, &["final-recipient", "original-recipient"]);
                }
            } else if report_type == "feedback-report" && part_type == "message/feedback-report" {
                kind = InboundKind::Complaint;
                report_recipient =
                    report_field(&fields, &["original-rcpt-to", "removal-recipient"]);
            }
        }
    } else if subject.trim().eq_ignore_ascii_case("unsubscribe") {
        kind = InboundKind::Unsubscribe;
    }
    let auto_submitted =
        top.header("auto-submitted").is_some_and(|value| !value.trim().eq_ignore_ascii_case("no"))
            || top.header("precedence").is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "bulk" | "junk" | "list" | "auto_reply"
                )
            });
    let text = body_text(&top, 0).unwrap_or_default().replace("\r\n", "\n");
    InboundEmail {
        id,
        thread,
        from: top
            .header("from")
            .and_then(mailbox)
            .unwrap_or_else(|| Mailbox::new("unknown@invalid")),
        to: top.header("to").map(mailboxes).unwrap_or_default(),
        cc: top.header("cc").map(mailboxes).unwrap_or_default(),
        subject: bounded(&subject, 300),
        text: bounded(text.trim(), MAX_TEXT_CHARS),
        received_at,
        in_reply_to,
        references,
        kind,
        auto_submitted,
        report_recipient,
    }
}

/// The address in the first of `names` among the report's fields, as `rfc822; address`.
fn report_field(fields: &str, names: &[&str]) -> Option<String> {
    names.iter().find_map(|wanted| {
        fields.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if !name.trim().eq_ignore_ascii_case(wanted) {
                return None;
            }
            let address = value.rsplit(';').next()?.trim().trim_matches(['<', '>']);
            address.contains('@').then(|| address.to_ascii_lowercase())
        })
    })
}

/// `value` as a header value: RFC 2047 encoded when it is not plain ASCII.
fn header_text(value: &str) -> String {
    if value.chars().all(|c| c.is_ascii() && !c.is_ascii_control()) {
        value.to_string()
    } else {
        format!("=?UTF-8?B?{}?=", base64::engine::general_purpose::STANDARD.encode(value))
    }
}

fn mailbox_text(mailbox: &Mailbox) -> String {
    match &mailbox.name {
        Some(name) if name.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ') => {
            format!("{name} <{}>", mailbox.address)
        }
        Some(name) => format!(
            "{} <{}>",
            header_text(&format!("\"{}\"", name.replace('"', ""))),
            mailbox.address
        ),
        None => mailbox.address.clone(),
    }
}

/// Refuses a value that would break out of its header.
fn check_header(name: &str, value: &str) -> Result<()> {
    if value.contains(['\r', '\n']) {
        return Err(DriverError::invalid(format!(
            "the {name} header may not contain a line break"
        )));
    }
    Ok(())
}

/// `text` quoted-printable, with CRLF line ends and lines of at most 76 characters.
fn quoted_printable_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    for (index, line) in text.replace("\r\n", "\n").split('\n').enumerate() {
        if index > 0 {
            out.push_str("\r\n");
        }
        let mut width = 0;
        let bytes = line.as_bytes();
        for (at, byte) in bytes.iter().enumerate() {
            let last = at + 1 == bytes.len();
            let encoded = match byte {
                b'=' => "=3D".to_string(),
                b' ' | b'\t' if last => format!("={byte:02X}"),
                33..=126 | b' ' | b'\t' => char::from(*byte).to_string(),
                _ => format!("={byte:02X}"),
            };
            if width + encoded.len() > 75 {
                out.push_str("=\r\n");
                width = 0;
            }
            width += encoded.len();
            out.push_str(&encoded);
        }
    }
    out
}

/// The wire form of `message` at `date` (RFC 5322), with CRLF line ends.
///
/// # Errors
///
/// Returns [`DriverError::Invalid`] for a header value with a line break or a message with
/// no recipient.
pub(crate) fn render_message(message: &OutboundEmail, date: DateTime<Utc>) -> Result<Vec<u8>> {
    if message.to.is_empty() {
        return Err(DriverError::invalid("a message needs a recipient"));
    }
    check_header("Subject", &message.subject)?;
    for mailbox in std::iter::once(&message.from).chain(&message.to) {
        check_header("address", &mailbox.address)?;
        if let Some(name) = &mailbox.name {
            check_header("address", name)?;
        }
    }
    let mut out = String::new();
    let mut push = |name: &str, value: &str| {
        out.push_str(name);
        out.push_str(": ");
        out.push_str(value);
        out.push_str("\r\n");
    };
    push("Date", &date.to_rfc2822());
    push("From", &mailbox_text(&message.from));
    push("To", &message.to.iter().map(mailbox_text).collect::<Vec<_>>().join(", "));
    push("Subject", &header_text(&message.subject));
    push("Message-ID", &format!("<{}>", message.message_id));
    if let Some(parent) = &message.in_reply_to {
        push("In-Reply-To", &format!("<{parent}>"));
    }
    if !message.references.is_empty() {
        push(
            "References",
            &message.references.iter().map(|id| format!("<{id}>")).collect::<Vec<_>>().join(" "),
        );
    }
    for (name, value) in &message.headers {
        check_header(name, value)?;
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(DriverError::invalid(format!("`{name}` is not a header name")));
        }
        push(name, value);
    }
    push("MIME-Version", "1.0");
    push("Content-Type", "text/plain; charset=utf-8");
    push("Content-Transfer-Encoding", "quoted-printable");
    out.push_str("\r\n");
    out.push_str(&quoted_printable_encode(&message.text));
    out.push_str("\r\n");
    Ok(out.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_reply_is_read_with_its_thread() {
        let raw = b"From: \"Ana W\" <ana@example.org>\r\nTo: hello@site.example\r\nSubject: =?UTF-8?Q?Caf=C3=A9_hours?=\r\n\
Message-ID: <m2@example.org>\r\nIn-Reply-To: <m1@site.example>\r\nReferences: <m0@example.org> <m1@site.example>\r\n\r\nWhen do you open?\r\n";
        let email = parse_message(raw, Utc::now(), "x");
        assert_eq!(email.id, "m2@example.org");
        assert_eq!(email.thread, "m0@example.org");
        assert_eq!(email.subject, "Café hours");
        assert_eq!(
            email.from,
            Mailbox { name: Some("Ana W".into()), address: "ana@example.org".into() }
        );
        assert_eq!(email.text, "When do you open?");
        assert_eq!(email.kind, InboundKind::Message);
    }

    #[test]
    fn multipart_prefers_plain_text_and_decodes_transfer_encodings() {
        let raw = b"From: a@example.org\r\nContent-Type: multipart/alternative; boundary=\"b1\"\r\n\r\n--b1\r\n\
Content-Type: text/html\r\n\r\n<p>html</p>\r\n--b1\r\nContent-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: base64\r\n\r\naMOpbGxv\r\n--b1--\r\n";
        assert_eq!(parse_message(raw, Utc::now(), "x").text, "héllo");
        let html = b"From: a@example.org\r\nContent-Type: text/html\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n<p>Tom &amp; Jerry=\r\n</p><p>caf=C3=A9</p>";
        assert_eq!(parse_message(html, Utc::now(), "x").text, "Tom & Jerry\ncafé");
    }

    #[test]
    fn delivery_and_feedback_reports_name_their_recipient() {
        let bounce = b"From: mailer-daemon@mx.example\r\nContent-Type: multipart/report; report-type=delivery-status; boundary=r\r\n\r\n\
--r\r\nContent-Type: text/plain\r\n\r\nDelivery failed\r\n--r\r\nContent-Type: message/delivery-status\r\n\r\n\
Reporting-MTA: dns; mx.example\r\n\r\nFinal-Recipient: rfc822; Gone@Example.org\r\nAction: failed\r\nStatus: 5.1.1\r\n--r--\r\n";
        let email = parse_message(bounce, Utc::now(), "x");
        assert_eq!(email.kind, InboundKind::Bounce);
        assert_eq!(email.report_recipient.as_deref(), Some("gone@example.org"));
        let complaint = b"From: fbl@isp.example\r\nContent-Type: multipart/report; report-type=feedback-report; boundary=f\r\n\r\n\
--f\r\nContent-Type: message/feedback-report\r\n\r\nFeedback-Type: abuse\r\nOriginal-Rcpt-To: <ana@example.org>\r\n--f--\r\n";
        let email = parse_message(complaint, Utc::now(), "x");
        assert_eq!(email.kind, InboundKind::Complaint);
        assert_eq!(email.report_recipient.as_deref(), Some("ana@example.org"));
    }

    #[test]
    fn rendering_refuses_header_injection_and_round_trips_text() {
        let mut message = OutboundEmail {
            message_id: "out-1@site.example".into(),
            from: Mailbox::new("hello@site.example"),
            to: vec![Mailbox::new("ana@example.org")],
            subject: "Re: Café hours".into(),
            text: format!("We open at 8.\n{}", "x".repeat(200)),
            in_reply_to: Some("m2@example.org".into()),
            references: vec!["m0@example.org".into(), "m2@example.org".into()],
            headers: Vec::new(),
        };
        let wire = render_message(&message, Utc::now()).unwrap();
        let back = parse_message(&wire, Utc::now(), "x");
        assert_eq!(back.subject, "Re: Café hours");
        assert_eq!(back.text, message.text);
        assert_eq!(back.thread, "m0@example.org");
        assert!(String::from_utf8(wire).unwrap().lines().all(|line| line.len() <= 998));
        message.subject = "hi\r\nBcc: everyone@example.org".into();
        assert!(render_message(&message, Utc::now()).is_err());
    }
}
