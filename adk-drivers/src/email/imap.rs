//! A read-only IMAP client (RFC 9051, RFC 3501; spec 3.5 §7.13): opens the mailbox with
//! EXAMINE, finds the messages after a UID, and fetches them with `BODY.PEEK[]`, so reading
//! never marks anything seen.

use chrono::{DateTime, Utc};

use super::imap_smtp::{Credentials, MailEndpoint, MailSecurity};
use super::tls::{MAX_LITERAL, MailConn, Tls};
use crate::error::{DriverError, Result};

const SERVICE: &str = "imap";

/// One untagged response: its text with each literal replaced by `{}`, and its literals.
pub(crate) struct Untagged {
    pub(crate) text: String,
    pub(crate) literals: Vec<Vec<u8>>,
}

/// A fetched message.
pub(crate) struct Fetched {
    pub(crate) uid: u32,
    pub(crate) received_at: Option<DateTime<Utc>>,
    pub(crate) raw: Vec<u8>,
}

/// What a read found.
pub(crate) struct Read {
    pub(crate) uid_validity: u32,
    pub(crate) messages: Vec<Fetched>,
}

struct Session {
    conn: MailConn,
    next_tag: u32,
}

impl Session {
    /// Sends `command` under a new tag and reads its responses up to the tagged one, which
    /// must be OK.
    async fn run(&mut self, command: &str, shown: &str) -> Result<Vec<Untagged>> {
        self.next_tag += 1;
        let tag = format!("a{}", self.next_tag);
        self.conn.write_line(&format!("{tag} {command}")).await?;
        let mut responses = Vec::new();
        loop {
            let (text, literals) = self.read_response().await?;
            if let Some(status) = text.strip_prefix(&format!("{tag} ")) {
                let ok = status.get(..2).is_some_and(|word| word.eq_ignore_ascii_case("OK"));
                if !ok {
                    let detail = crate::outbound::bounded(status, 200);
                    if shown == "LOGIN" {
                        return Err(DriverError::Unauthorized {
                            service: SERVICE.into(),
                            reason: detail,
                        });
                    }
                    return Err(DriverError::unavailable(SERVICE, format!("{shown}: {detail}")));
                }
                return Ok(responses);
            }
            if text.starts_with('*') {
                responses.push(Untagged { text, literals });
            }
        }
    }

    /// One response line with its literals.
    async fn read_response(&mut self) -> Result<(String, Vec<Vec<u8>>)> {
        let mut text = String::new();
        let mut literals = Vec::new();
        loop {
            let line = self.conn.read_line().await?;
            match literal_length(&line) {
                Some(length) => {
                    if length > MAX_LITERAL {
                        return Err(DriverError::protocol(
                            SERVICE,
                            "a message is larger than 25 MiB",
                        ));
                    }
                    text.push_str(&line[..line.rfind('{').unwrap_or(line.len())]);
                    text.push_str("{}");
                    literals.push(self.conn.read_bytes(length).await?);
                }
                None => {
                    text.push_str(&line);
                    return Ok((text, literals));
                }
            }
        }
    }
}

/// The length of the literal a line announces at its end, as `{123}`.
fn literal_length(line: &str) -> Option<usize> {
    let inner = line.strip_suffix('}')?;
    let open = inner.rfind('{')?;
    inner[open + 1..].trim_end_matches('+').parse().ok()
}

/// `text` as an IMAP quoted string.
fn quoted(text: &str) -> Result<String> {
    if text.contains(['\r', '\n', '\0']) {
        return Err(DriverError::invalid("an IMAP credential may not contain a line break"));
    }
    Ok(format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\"")))
}

/// The number after `key ` in `text`, such as `UIDVALIDITY 3857529045`.
fn number_after(text: &str, key: &str) -> Option<u32> {
    let upper = text.to_ascii_uppercase();
    let at = upper.find(key)? + key.len();
    text[at..].trim_start().split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// The `INTERNALDATE` of a FETCH response, as `"17-Jul-1996 02:44:25 -0700"`.
fn internal_date(text: &str) -> Option<DateTime<Utc>> {
    let upper = text.to_ascii_uppercase();
    let at = upper.find("INTERNALDATE \"")? + "INTERNALDATE \"".len();
    let end = text[at..].find('"')? + at;
    DateTime::parse_from_str(text[at..end].trim(), "%d-%b-%Y %H:%M:%S %z")
        .ok()
        .map(|date| date.with_timezone(&Utc))
}

/// Reads at most `limit` messages after UID `after` from `mailbox`, when the mailbox's
/// UIDVALIDITY is still `validity`; from the start otherwise.
pub(crate) async fn read(
    endpoint: &MailEndpoint,
    credentials: &Credentials,
    tls: &Tls,
    mailbox: &str,
    after: Option<(u32, u32)>,
    limit: usize,
) -> Result<Read> {
    let implicit = (endpoint.security == MailSecurity::Tls).then_some(tls);
    let conn =
        MailConn::connect(SERVICE, &endpoint.host, endpoint.port, implicit, endpoint.timeout)
            .await?;
    let mut session = Session { conn, next_tag: 0 };
    let (greeting, _) = session.read_response().await?;
    if !greeting.to_ascii_uppercase().starts_with("* OK") {
        return Err(DriverError::unavailable(
            SERVICE,
            format!("greeting: {}", crate::outbound::bounded(&greeting, 120)),
        ));
    }
    if endpoint.security == MailSecurity::StartTls {
        session.run("STARTTLS", "STARTTLS").await?;
        session.conn.upgrade(tls).await?;
    }
    if !session.conn.is_tls() && endpoint.security != MailSecurity::Plain {
        return Err(DriverError::refused("refusing to log in to IMAP without TLS"));
    }
    let login = format!(
        "LOGIN {} {}",
        quoted(&credentials.username)?,
        quoted(credentials.password.expose())?
    );
    session.run(&login, "LOGIN").await?;
    let examined = session.run(&format!("EXAMINE {}", quoted(mailbox)?), "EXAMINE").await?;
    let uid_validity = examined
        .iter()
        .find_map(|response| number_after(&response.text, "UIDVALIDITY"))
        .ok_or_else(|| DriverError::protocol(SERVICE, "the server gave no UIDVALIDITY"))?;
    let last = match after {
        Some((validity, uid)) if validity == uid_validity => uid,
        _ => 0,
    };
    let found = session.run(&format!("UID SEARCH UID {}:*", last + 1), "UID SEARCH").await?;
    let mut uids: Vec<u32> = found
        .iter()
        .filter(|response| response.text.to_ascii_uppercase().starts_with("* SEARCH"))
        .flat_map(|response| {
            response.text.split_whitespace().skip(2).filter_map(|word| word.parse::<u32>().ok())
        })
        .filter(|uid| *uid > last)
        .collect();
    uids.sort_unstable();
    uids.dedup();
    uids.truncate(limit);
    let mut messages = Vec::new();
    if !uids.is_empty() {
        let set = uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        let fetched = session
            .run(&format!("UID FETCH {set} (UID INTERNALDATE BODY.PEEK[])"), "UID FETCH")
            .await?;
        for response in fetched {
            let Some(uid) = number_after(&response.text, "UID") else { continue };
            let Some(raw) = response.literals.into_iter().next() else { continue };
            if uids.contains(&uid) {
                messages.push(Fetched { uid, received_at: internal_date(&response.text), raw });
            }
        }
        messages.sort_by_key(|message| message.uid);
    }
    let _ = session.run("LOGOUT", "LOGOUT").await;
    Ok(Read { uid_validity, messages })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_are_read_for_their_numbers_and_dates() {
        assert_eq!(literal_length("* 1 FETCH (UID 5 BODY[] {342}"), Some(342));
        assert_eq!(literal_length("* 1 FETCH (UID 5)"), None);
        assert_eq!(
            number_after("* OK [UIDVALIDITY 3857529045] UIDs valid", "UIDVALIDITY"),
            Some(3_857_529_045)
        );
        assert_eq!(number_after("* 1 FETCH (FLAGS () UID 17 INTERNALDATE", "UID"), Some(17));
        let date =
            internal_date("* 1 FETCH (UID 5 INTERNALDATE \"17-Jul-1996 02:44:25 -0700\" BODY[] {}")
                .unwrap();
        assert_eq!(date.to_rfc3339(), "1996-07-17T09:44:25+00:00");
        assert_eq!(quoted("pa\"ss\\").unwrap(), "\"pa\\\"ss\\\\\"");
        assert!(quoted("a\r\nb").is_err());
    }
}
