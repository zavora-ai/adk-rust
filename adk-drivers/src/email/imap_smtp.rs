//! Email over IMAP and SMTP (spec 3.5 §7.13; feature `imap-smtp`).

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rustls::pki_types::CertificateDer;

use super::tls::Tls;
use super::{
    DeliveryEvent, DeliveryLog, DeliveryStats, EmailDriver, InboxPage, OutboundEmail, SentEmail,
    imap, mime, smtp,
};
use crate::error::{DriverError, Result};

/// How a mail connection is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MailSecurity {
    /// TLS from the first byte: IMAPS on 993, SMTP submission on 465.
    Tls,
    /// Plain first, upgraded with STARTTLS before anything is sent: IMAP on 143, SMTP
    /// submission on 587. A server that does not offer it is refused.
    StartTls,
    /// No TLS: loopback only, for a local relay or a test server.
    Plain,
}

/// A mail server's address and security.
///
/// # Example
///
/// ```rust
/// use adk_drivers::email::MailEndpoint;
///
/// assert!(MailEndpoint::tls("imap.example.com", 993).is_ok());
/// assert!(MailEndpoint::plain("127.0.0.1", 1143).is_ok());
/// assert!(MailEndpoint::plain("mail.example.com", 25).is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MailEndpoint {
    /// The host name, which TLS verifies.
    pub host: String,
    /// The port.
    pub port: u16,
    /// How the connection is secured.
    pub security: MailSecurity,
    /// How long one step may take.
    pub timeout: Duration,
}

impl MailEndpoint {
    fn new(host: &str, port: u16, security: MailSecurity) -> Result<Self> {
        let host = host.trim().to_string();
        if host.is_empty() {
            return Err(DriverError::invalid("a mail server needs a host"));
        }
        Ok(Self { host, port, security, timeout: Duration::from_secs(30) })
    }

    /// `host:port` under TLS from the start.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for an empty host.
    pub fn tls(host: &str, port: u16) -> Result<Self> {
        Self::new(host, port, MailSecurity::Tls)
    }

    /// `host:port` upgraded with STARTTLS.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for an empty host.
    pub fn starttls(host: &str, port: u16) -> Result<Self> {
        Self::new(host, port, MailSecurity::StartTls)
    }

    /// `host:port` without TLS, on loopback only.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for a host that is not loopback: credentials and
    /// mail never cross a network in clear text.
    pub fn plain(host: &str, port: u16) -> Result<Self> {
        let loopback = host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if !loopback {
            return Err(DriverError::invalid(format!(
                "`{host}` is not loopback; use TLS or STARTTLS for a mail server on a network"
            )));
        }
        Self::new(host, port, MailSecurity::Plain)
    }

    /// Sets how long one step may take; 30 s by default.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// A password that never appears in `Debug`.
#[derive(Clone)]
pub(crate) struct Secret(String);

impl Secret {
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

/// The account's user name and password, from the credential broker.
#[derive(Debug, Clone)]
pub(crate) struct Credentials {
    pub(crate) username: String,
    pub(crate) password: Secret,
}

/// An email account over IMAP and SMTP (spec 3.5 §7.13): any mailbox provider or mail
/// server.
///
/// Reading opens the mailbox read-only (EXAMINE) and fetches with `BODY.PEEK[]`, so the
/// owner's mail client still sees new mail as unread; its cursor is the mailbox's
/// UIDVALIDITY and the last UID read. Sending submits one message per call. Sends,
/// refused recipients, and the bounces and complaints seen in the inbox are counted in
/// process for [`delivery_stats`](EmailDriver::delivery_stats).
///
/// The password comes from the credential broker (A-I19); it never appears in `Debug`,
/// errors or logs, and never crosses a network without TLS.
///
/// # Example
///
/// ```rust,no_run
/// use adk_drivers::email::{EmailDriver, ImapSmtpEmail, MailEndpoint};
///
/// # async fn example(password: String) -> adk_drivers::Result<()> {
/// let email = ImapSmtpEmail::new(
///     MailEndpoint::tls("imap.example.com", 993)?,
///     MailEndpoint::starttls("smtp.example.com", 587)?,
///     "hello@bakery.example",
///     password,
/// )?;
/// let page = email.inbox(None, 20).await?;
/// # let _ = page;
/// # Ok(())
/// # }
/// ```
pub struct ImapSmtpEmail {
    imap: MailEndpoint,
    smtp: MailEndpoint,
    credentials: Credentials,
    mailbox: String,
    helo: String,
    tls: Tls,
    log: DeliveryLog,
}

impl std::fmt::Debug for ImapSmtpEmail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImapSmtpEmail")
            .field("imap", &self.imap)
            .field("smtp", &self.smtp)
            .field("username", &self.credentials.username)
            .field("mailbox", &self.mailbox)
            .finish_non_exhaustive()
    }
}

impl ImapSmtpEmail {
    /// The account `username` with `password`, read through `imap` and sent through
    /// `smtp`, trusting the system's root certificates.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for an empty user name or password.
    pub fn new(
        imap: MailEndpoint,
        smtp: MailEndpoint,
        username: &str,
        password: impl Into<String>,
    ) -> Result<Self> {
        let password = password.into();
        if username.trim().is_empty() || password.is_empty() {
            return Err(DriverError::invalid("an email account needs a user name and a password"));
        }
        let helo = username
            .rsplit('@')
            .next()
            .filter(|domain| domain.contains('.'))
            .unwrap_or("localhost")
            .to_string();
        Ok(Self {
            imap,
            smtp,
            credentials: Credentials {
                username: username.trim().to_string(),
                password: Secret(password),
            },
            mailbox: "INBOX".to_string(),
            helo,
            tls: Tls::new(&[])?,
            log: DeliveryLog::default(),
        })
    }

    /// Reads `mailbox` instead of `INBOX`.
    #[must_use]
    pub fn with_mailbox(mut self, mailbox: impl Into<String>) -> Self {
        self.mailbox = mailbox.into();
        self
    }

    /// Trusts only the root certificate `der` (DER-encoded), as for a private CA or a test
    /// server.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for a certificate rustls does not accept.
    pub fn with_root_certificate(mut self, der: Vec<u8>) -> Result<Self> {
        self.tls = Tls::new(&[CertificateDer::from(der)])?;
        Ok(self)
    }
}

/// The cursor's UIDVALIDITY and UID.
fn parse_cursor(cursor: &str) -> Result<(u32, u32)> {
    let parsed = cursor
        .split_once(':')
        .and_then(|(validity, uid)| Some((validity.parse().ok()?, uid.parse().ok()?)));
    parsed
        .ok_or_else(|| DriverError::invalid(format!("`{cursor}` is not a cursor this driver gave")))
}

#[async_trait]
impl EmailDriver for ImapSmtpEmail {
    fn name(&self) -> &str {
        "imap-smtp"
    }

    async fn inbox(&self, cursor: Option<&str>, limit: usize) -> Result<InboxPage> {
        let after = cursor.map(parse_cursor).transpose()?;
        let read =
            imap::read(&self.imap, &self.credentials, &self.tls, &self.mailbox, after, limit)
                .await?;
        let last = match (read.messages.last(), after) {
            (Some(message), _) => message.uid,
            (None, Some((validity, uid))) if validity == read.uid_validity => uid,
            (None, _) => 0,
        };
        let messages: Vec<_> = read
            .messages
            .iter()
            .map(|fetched| {
                let fallback = format!("{}.{}@imap.invalid", fetched.uid, read.uid_validity);
                mime::parse_message(
                    &fetched.raw,
                    fetched.received_at.unwrap_or_else(Utc::now),
                    &fallback,
                )
            })
            .collect();
        self.log.record_reports(&messages);
        Ok(InboxPage { messages, cursor: Some(format!("{}:{last}", read.uid_validity)) })
    }

    async fn send(&self, message: &OutboundEmail) -> Result<SentEmail> {
        let at = Utc::now();
        let wire = mime::render_message(message, at)?;
        let recipients: Vec<String> =
            message.to.iter().map(|mailbox| mailbox.address.clone()).collect();
        let submitted = smtp::submit(
            &self.smtp,
            &self.credentials,
            &self.tls,
            &self.helo,
            &message.from.address,
            &recipients,
            &wire,
        )
        .await;
        let submitted = match submitted {
            Ok(submitted) => submitted,
            Err(error) => {
                if matches!(error, DriverError::Refused { .. }) {
                    for _ in &recipients {
                        self.log.record(DeliveryEvent::Rejected, at);
                    }
                }
                return Err(error);
            }
        };
        for _ in &submitted.rejected {
            self.log.record(DeliveryEvent::Rejected, at);
        }
        self.log.record(DeliveryEvent::Sent, at);
        Ok(SentEmail { message_id: message.message_id.clone(), accepted: submitted.accepted, at })
    }

    async fn delivery_stats(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<DeliveryStats> {
        Ok(self.log.stats(from, to))
    }
}
