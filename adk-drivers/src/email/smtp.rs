//! An SMTP submission client (RFC 5321, RFC 3207 STARTTLS, RFC 4954 AUTH; spec 3.5 §7.13).

use base64::Engine as _;

use super::imap_smtp::{Credentials, MailEndpoint, MailSecurity};
use super::tls::{MailConn, Tls};
use crate::error::{DriverError, Result};

const SERVICE: &str = "smtp";

/// A reply: its code and its text lines.
struct Reply {
    code: u16,
    lines: Vec<String>,
}

impl Reply {
    fn text(&self) -> String {
        self.lines.join(" ")
    }
}

async fn read_reply(conn: &mut MailConn) -> Result<Reply> {
    let mut lines = Vec::new();
    loop {
        let line = conn.read_line().await?;
        let code = line.get(..3).and_then(|code| code.parse::<u16>().ok()).ok_or_else(|| {
            DriverError::protocol(
                SERVICE,
                format!("`{}` is not an SMTP reply", crate::outbound::bounded(&line, 80)),
            )
        })?;
        let more = line.as_bytes().get(3) == Some(&b'-');
        lines.push(line.get(4..).unwrap_or_default().to_string());
        if !more {
            return Ok(Reply { code, lines });
        }
    }
}

/// Sends `command` and reads the reply, which must be `expected`'s class (2 or 3).
async fn command(conn: &mut MailConn, command: &str, expected: u16, shown: &str) -> Result<Reply> {
    conn.write_line(command).await?;
    let reply = read_reply(conn).await?;
    if reply.code / 100 != expected {
        return Err(failure(shown, &reply));
    }
    Ok(reply)
}

fn failure(what: &str, reply: &Reply) -> DriverError {
    let detail = format!("{what}: {} {}", reply.code, crate::outbound::bounded(&reply.text(), 200));
    match reply.code {
        535 | 530 => DriverError::Unauthorized { service: SERVICE.into(), reason: detail },
        400..=499 => DriverError::unavailable(SERVICE, detail),
        _ => DriverError::refused(detail),
    }
}

/// The outcome of one submission: accepted and rejected recipients.
pub(crate) struct Submitted {
    pub(crate) accepted: Vec<String>,
    pub(crate) rejected: Vec<String>,
}

/// `message` with lines starting with a dot doubled and the final dot line (RFC 5321
/// §4.5.2).
fn dot_stuffed(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + 16);
    let mut line_start = true;
    for byte in message {
        if line_start && *byte == b'.' {
            out.push(b'.');
        }
        out.push(*byte);
        line_start = *byte == b'\n';
    }
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b".\r\n");
    out
}

/// Submits `message` from `from` to `recipients` through `endpoint`.
pub(crate) async fn submit(
    endpoint: &MailEndpoint,
    credentials: &Credentials,
    tls: &Tls,
    helo: &str,
    from: &str,
    recipients: &[String],
    message: &[u8],
) -> Result<Submitted> {
    let implicit = (endpoint.security == MailSecurity::Tls).then_some(tls);
    let mut conn =
        MailConn::connect(SERVICE, &endpoint.host, endpoint.port, implicit, endpoint.timeout)
            .await?;
    let greeting = read_reply(&mut conn).await?;
    if greeting.code != 220 {
        return Err(failure("greeting", &greeting));
    }
    let mut extensions = command(&mut conn, &format!("EHLO {helo}"), 2, "EHLO").await?.lines;
    if endpoint.security == MailSecurity::StartTls {
        if !extensions.iter().any(|line| line.eq_ignore_ascii_case("STARTTLS")) {
            return Err(DriverError::refused(
                "the SMTP server does not offer STARTTLS; refusing to send in clear text",
            ));
        }
        command(&mut conn, "STARTTLS", 2, "STARTTLS").await?;
        conn.upgrade(tls).await?;
        extensions = command(&mut conn, &format!("EHLO {helo}"), 2, "EHLO").await?.lines;
    }
    if !conn.is_tls() && endpoint.security != MailSecurity::Plain {
        return Err(DriverError::refused("refusing to authenticate without TLS"));
    }
    let mechanisms: Vec<String> = extensions
        .iter()
        .find_map(|line| {
            let upper = line.to_ascii_uppercase();
            upper.strip_prefix("AUTH ").or_else(|| upper.strip_prefix("AUTH=")).map(str::to_string)
        })
        .map(|mechanisms| mechanisms.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    let engine = &base64::engine::general_purpose::STANDARD;
    if mechanisms.iter().any(|m| m == "PLAIN") {
        let token =
            engine.encode(format!("\0{}\0{}", credentials.username, credentials.password.expose()));
        command(&mut conn, &format!("AUTH PLAIN {token}"), 2, "AUTH PLAIN").await?;
    } else if mechanisms.iter().any(|m| m == "LOGIN") {
        command(&mut conn, "AUTH LOGIN", 3, "AUTH LOGIN").await?;
        command(&mut conn, &engine.encode(&credentials.username), 3, "AUTH LOGIN user").await?;
        command(&mut conn, &engine.encode(credentials.password.expose()), 2, "AUTH LOGIN").await?;
    } else {
        return Err(DriverError::refused(
            "the SMTP server offers neither AUTH PLAIN nor AUTH LOGIN",
        ));
    }
    command(&mut conn, &format!("MAIL FROM:<{from}>"), 2, "MAIL FROM").await?;
    let mut submitted = Submitted { accepted: Vec::new(), rejected: Vec::new() };
    for recipient in recipients {
        if recipient.contains(['\r', '\n', '<', '>']) {
            submitted.rejected.push(recipient.clone());
            continue;
        }
        conn.write_line(&format!("RCPT TO:<{recipient}>")).await?;
        let reply = read_reply(&mut conn).await?;
        if reply.code / 100 == 2 {
            submitted.accepted.push(recipient.clone());
        } else {
            tracing::warn!(smtp.code = reply.code, "the SMTP server refused a recipient");
            submitted.rejected.push(recipient.clone());
        }
    }
    if submitted.accepted.is_empty() {
        let _ = command(&mut conn, "RSET", 2, "RSET").await;
        let _ = conn.write_line("QUIT").await;
        return Err(DriverError::refused("the SMTP server accepted no recipient"));
    }
    command(&mut conn, "DATA", 3, "DATA").await?;
    conn.write(&dot_stuffed(message)).await?;
    let reply = read_reply(&mut conn).await?;
    if reply.code / 100 != 2 {
        return Err(failure("message", &reply));
    }
    let _ = conn.write_line("QUIT").await;
    Ok(submitted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_dots_are_doubled_and_the_message_ends_with_a_dot_line() {
        assert_eq!(dot_stuffed(b".a\r\nb\r\n..c"), b"..a\r\nb\r\n...c\r\n.\r\n");
    }
}
