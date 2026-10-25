//! Email drivers (spec 3.5 §7.13): the inbox in the public partition, `reply_in_thread`,
//! sequences and newsletters in the back office, and delivery statistics as a metric
//! source.
//!
//! | Tool | Effect class | Side | Default cap |
//! |---|---|---|---|
//! | `email_list_inbox` | `read` | public | Read |
//! | `email_reply_in_thread` | `send_external` | public | Within playbook |
//! | `email_send_sequence_step` | `send_external` | back office | Within playbook and consent |
//! | `email_send_newsletter` | `send_external` | back office | Within playbook and consent |
//! | `email_delivery_stats` | `read` | back office | Read |
//! | `email_check_domain` | `read` | back office | Read |
//!
//! An [`EmailDriver`] only moves mail. [`EmailOutbox`] keeps the rules over any of them:
//!
//! | Rule | Spec |
//! |---|---|
//! | Every outbound message ends with the [`Disclosure`] line naming the owner and saying an AI agent wrote it | A-I17 |
//! | An unsubscribe or a revocation is read before every send, so it takes effect before the next one; an unsubscribe mail, a bounce and a complaint are recorded as the inbox is read | A-I17 |
//! | SPF, DKIM and DMARC are checked before the first send; sequences and newsletters are refused until all three pass | §7.13 |
//! | A sequence step or newsletter goes only to a recipient with a live consent receipt for its scope, checked at send time | A-I8 |
//! | A reply goes to the thread's sender only — no new recipients, no attachments — from a playbook template, at a rate limit, never to an automatic message | §7.10 |
//!
//! | Adapter | Feature |
//! |---|---|
//! | [`InMemoryEmail`] | — |
//! | `ImapSmtpEmail`: IMAP (RFC 9051) to read, SMTP (RFC 5321) to send | `imap-smtp` |

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use adk_core::Tool;
use adk_kernel::drivers::EffectClass;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::class::{AutonomyCap, Side, ToolSpec};
use crate::error::{DriverError, Result};
use crate::metrics::{KpiSource, unknown_metric};
use crate::outbound::{
    ConsentCheck, Disclosure, InMemoryConsent, InMemorySuppressions, Limiter, RateLimit,
    ReplyTemplates, Suppression, SuppressionList, SuppressionScope, ThreadReplier, fields_arg,
    str_arg,
};
use crate::tools::DriverTool;

mod auth;
#[cfg(feature = "imap-smtp")]
mod imap;
#[cfg(feature = "imap-smtp")]
mod imap_smtp;
pub(crate) mod mime;
#[cfg(feature = "imap-smtp")]
mod smtp;
#[cfg(feature = "imap-smtp")]
mod tls;

#[cfg(feature = "system-dns")]
pub use auth::SystemTxtLookup;
pub use auth::{AuthCheck, DomainAuthCheck, DomainAuthReport, SenderPolicy, StaticTxt, TxtLookup};
#[cfg(feature = "imap-smtp")]
pub use imap_smtp::{ImapSmtpEmail, MailEndpoint, MailSecurity};

/// How long a passing domain check stays good before it is run again.
const AUTH_FRESHNESS: chrono::Duration = chrono::Duration::hours(24);

/// A mailbox: an address and an optional display name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Mailbox {
    /// The display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The address.
    pub address: String,
}

impl Mailbox {
    /// The mailbox `address`.
    pub fn new(address: impl Into<String>) -> Self {
        Self { name: None, address: address.into().trim().to_string() }
    }

    /// Sets the display name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// What an inbound message is (spec 3.5 §7.13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum InboundKind {
    /// A message from a person.
    Message,
    /// A delivery report that a message failed (RFC 3464).
    Bounce,
    /// A complaint report (RFC 5965).
    Complaint,
    /// A request to unsubscribe, by the `mailto` unsubscribe convention (RFC 2369).
    Unsubscribe,
}

/// A message in the inbox (spec 3.5 §7.13): untrusted input, read in the public partition
/// only (A-I3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct InboundEmail {
    /// Its `Message-ID`, without angle brackets.
    pub id: String,
    /// Its thread: the first message id of `References`, or `In-Reply-To`, or its own.
    pub thread: String,
    /// The sender.
    pub from: Mailbox,
    /// The `To` recipients.
    pub to: Vec<Mailbox>,
    /// The `Cc` recipients.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<Mailbox>,
    /// The subject, at most 300 characters.
    pub subject: String,
    /// The plain text, at most 32,000 characters.
    pub text: String,
    /// When it arrived.
    pub received_at: DateTime<Utc>,
    /// The message it answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    /// Its `References`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
    /// What it is.
    pub kind: InboundKind,
    /// Whether software sent it on its own (RFC 3834), so it gets no reply.
    #[serde(default)]
    pub auto_submitted: bool,
    /// For a bounce or complaint, the recipient it is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_recipient: Option<String>,
}

impl InboundEmail {
    /// A plain message from `from` with `subject` and `text`, its own thread, for tests and
    /// the in-memory driver.
    pub fn message(from: Mailbox, subject: impl Into<String>, text: impl Into<String>) -> Self {
        let id = format!("{}@inbound.invalid", Uuid::new_v4().simple());
        Self {
            thread: id.clone(),
            id,
            from,
            to: Vec::new(),
            cc: Vec::new(),
            subject: subject.into(),
            text: text.into(),
            received_at: Utc::now(),
            in_reply_to: None,
            references: Vec::new(),
            kind: InboundKind::Message,
            auto_submitted: false,
            report_recipient: None,
        }
    }

    /// Reads a raw RFC 5322 message that arrived at `received_at`.
    pub fn parse(raw: &[u8], received_at: DateTime<Utc>) -> Self {
        mime::parse_message(
            raw,
            received_at,
            &format!("{}@inbound.invalid", Uuid::new_v4().simple()),
        )
    }
}

/// A page of the inbox, after a cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct InboxPage {
    /// The messages, oldest first.
    pub messages: Vec<InboundEmail>,
    /// The cursor after the last message, for the next read; unchanged when there are none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// A message to send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct OutboundEmail {
    /// Its `Message-ID`, without angle brackets.
    pub message_id: String,
    /// The sender: the site's address.
    pub from: Mailbox,
    /// The recipients.
    pub to: Vec<Mailbox>,
    /// The subject.
    pub subject: String,
    /// The plain text.
    pub text: String,
    /// The message it answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    /// Its `References`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
    /// Further headers, such as `List-Unsubscribe`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<(String, String)>,
}

impl OutboundEmail {
    /// The message's wire form at `date` (RFC 5322), with CRLF line ends, as SMTP sends it.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for a header value with a line break or a message
    /// with no recipient.
    pub fn to_wire(&self, date: DateTime<Utc>) -> Result<Vec<u8>> {
        mime::render_message(self, date)
    }
}

/// A message the driver handed on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct SentEmail {
    /// Its `Message-ID`.
    pub message_id: String,
    /// The recipients the server accepted.
    pub accepted: Vec<String>,
    /// When.
    pub at: DateTime<Utc>,
}

/// Delivery statistics over a window (spec 3.5 §7.12, §7.13).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DeliveryStats {
    /// Messages accepted by the sending server.
    pub sent: u64,
    /// Recipients the sending server refused.
    pub rejected: u64,
    /// Bounces received.
    pub bounced: u64,
    /// Complaints received.
    pub complaints: u64,
}

/// An email service (spec 3.5 §7.13): reads the inbox, sends one message, counts deliveries.
///
/// It only moves mail; [`EmailOutbox`] keeps the rules. An adapter refuses nothing a rule
/// would, so the conformance suite can show it moves mail faithfully.
#[async_trait]
pub trait EmailDriver: Send + Sync {
    /// The adapter's name.
    fn name(&self) -> &str;

    /// The messages that arrived after `cursor` (from the start when `None`), oldest first,
    /// at most `limit`.
    ///
    /// # Errors
    ///
    /// Returns the service's errors.
    async fn inbox(&self, cursor: Option<&str>, limit: usize) -> Result<InboxPage>;

    /// Sends `message`.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Refused`] when the server accepted no recipient, and the
    /// service's errors.
    async fn send(&self, message: &OutboundEmail) -> Result<SentEmail>;

    /// Delivery statistics over `[from, to)`.
    ///
    /// # Errors
    ///
    /// Returns the service's errors.
    async fn delivery_stats(&self, from: DateTime<Utc>, to: DateTime<Utc>)
    -> Result<DeliveryStats>;
}

/// Sends and receipts counted in process, for adapters whose service does not count them.
#[derive(Debug, Default)]
pub(crate) struct DeliveryLog {
    events: Mutex<Vec<(DateTime<Utc>, DeliveryEvent)>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryEvent {
    Sent,
    Rejected,
    Bounced,
    Complained,
}

impl DeliveryLog {
    pub(crate) fn record(&self, event: DeliveryEvent, at: DateTime<Utc>) {
        self.events.lock().unwrap_or_else(PoisonError::into_inner).push((at, event));
    }

    /// Records the bounces and complaints of `messages`, once each.
    pub(crate) fn record_reports(&self, messages: &[InboundEmail]) {
        for message in messages {
            match message.kind {
                InboundKind::Bounce => self.record(DeliveryEvent::Bounced, message.received_at),
                InboundKind::Complaint => {
                    self.record(DeliveryEvent::Complained, message.received_at)
                }
                InboundKind::Message | InboundKind::Unsubscribe => {}
            }
        }
    }

    pub(crate) fn stats(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> DeliveryStats {
        let events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        let mut stats = DeliveryStats::default();
        for (_, event) in events.iter().filter(|(at, _)| *at >= from && *at < to) {
            match event {
                DeliveryEvent::Sent => stats.sent += 1,
                DeliveryEvent::Rejected => stats.rejected += 1,
                DeliveryEvent::Bounced => stats.bounced += 1,
                DeliveryEvent::Complained => stats.complaints += 1,
            }
        }
        stats
    }
}

/// An email service in process memory (spec 3.5 §7.13): messages delivered to it with
/// [`deliver`](Self::deliver), and what it sent, for tests and as the hermetic reference
/// of the [`EmailDriver`] conformance suite.
///
/// # Example
///
/// ```rust
/// use adk_drivers::email::{EmailDriver, InMemoryEmail, InboundEmail, Mailbox};
///
/// # futures::executor::block_on(async {
/// let email = InMemoryEmail::new();
/// email.deliver(InboundEmail::message(Mailbox::new("ana@example.org"), "Hours?", "When do you open?"));
/// let page = email.inbox(None, 10).await.unwrap();
/// assert_eq!(page.messages.len(), 1);
/// assert!(email.inbox(page.cursor.as_deref(), 10).await.unwrap().messages.is_empty());
/// # });
/// ```
#[derive(Debug, Default)]
pub struct InMemoryEmail {
    inbox: Mutex<Vec<InboundEmail>>,
    sent: Mutex<Vec<OutboundEmail>>,
    refuse: Mutex<Vec<String>>,
    log: DeliveryLog,
}

impl InMemoryEmail {
    /// An empty mailbox.
    pub fn new() -> Self {
        Self::default()
    }

    /// Delivers `message` to the inbox.
    pub fn deliver(&self, message: InboundEmail) {
        self.log.record_reports(std::slice::from_ref(&message));
        self.inbox.lock().unwrap_or_else(PoisonError::into_inner).push(message);
    }

    /// Everything sent, in order.
    pub fn sent(&self) -> Vec<OutboundEmail> {
        self.sent.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Makes the server refuse `address` from now on.
    pub fn refuse(&self, address: &str) {
        self.refuse
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(address.to_ascii_lowercase());
    }
}

#[async_trait]
impl EmailDriver for InMemoryEmail {
    fn name(&self) -> &str {
        "in-memory"
    }

    async fn inbox(&self, cursor: Option<&str>, limit: usize) -> Result<InboxPage> {
        let after = match cursor {
            Some(cursor) => cursor.parse::<usize>().map_err(|_| {
                DriverError::invalid(format!("`{cursor}` is not a cursor this driver gave"))
            })?,
            None => 0,
        };
        let inbox = self.inbox.lock().unwrap_or_else(PoisonError::into_inner);
        let messages: Vec<InboundEmail> = inbox.iter().skip(after).take(limit).cloned().collect();
        Ok(InboxPage { cursor: Some((after + messages.len()).to_string()), messages })
    }

    async fn send(&self, message: &OutboundEmail) -> Result<SentEmail> {
        let refused = self.refuse.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let at = Utc::now();
        let mut accepted = Vec::new();
        for recipient in &message.to {
            if refused.contains(&recipient.address.to_ascii_lowercase()) {
                self.log.record(DeliveryEvent::Rejected, at);
            } else {
                accepted.push(recipient.address.clone());
            }
        }
        if accepted.is_empty() {
            return Err(DriverError::refused("the server accepted no recipient"));
        }
        mime::render_message(message, at)?;
        self.sent.lock().unwrap_or_else(PoisonError::into_inner).push(message.clone());
        self.log.record(DeliveryEvent::Sent, at);
        Ok(SentEmail { message_id: message.message_id.clone(), accepted, at })
    }

    async fn delivery_stats(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<DeliveryStats> {
        Ok(self.log.stats(from, to))
    }
}

/// What kind of send, for the rules (spec 3.5 §7.13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum SendKind {
    /// A reply in the thread of a person who wrote.
    Reply,
    /// A step of a nurture sequence: bulk.
    Sequence,
    /// A newsletter: bulk.
    Newsletter,
}

impl SendKind {
    /// Whether it is bulk sending, refused until SPF, DKIM and DMARC pass.
    pub fn is_bulk(self) -> bool {
        matches!(self, Self::Sequence | Self::Newsletter)
    }
}

/// A thread the inbox read, as the outbox remembers it for replies.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KnownThread {
    sender: Mailbox,
    subject: String,
    last_message: String,
    references: Vec<String>,
    automatic: bool,
}

/// One recipient's outcome in a newsletter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct NewsletterReport {
    /// The recipients it was sent to.
    pub sent: Vec<String>,
    /// The recipients skipped, with the rule that skipped each.
    pub skipped: Vec<(String, String)>,
}

/// Makes the unsubscribe link of a recipient: an opaque link the edge serves, never the
/// address itself in the URL.
pub type UnsubscribeLink = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// The rules over an [`EmailDriver`] (spec 3.5 §7.10, §7.13, A-I8, A-I17); see the module
/// documentation for the table.
///
/// # Example
///
/// ```rust
/// use std::collections::BTreeMap;
/// use std::sync::Arc;
/// use adk_drivers::email::{EmailOutbox, InMemoryEmail, InboundEmail, Mailbox, SenderPolicy, StaticTxt};
/// use adk_drivers::{Disclosure, ReplyTemplates};
///
/// # futures::executor::block_on(async {
/// let mail = Arc::new(InMemoryEmail::new());
/// let outbox = EmailOutbox::builder(mail.clone(), Mailbox::new("hello@bakery.example"), Disclosure::new("Wanjiru's Bakery").unwrap())
///     .domain_auth(Arc::new(StaticTxt::new()), SenderPolicy::new("bakery.example", "mail", "include:_spf.mail.example"))
///     .templates(ReplyTemplates::new().template("hours", "Hi {name}, we open at 7am."))
///     .build();
/// mail.deliver(InboundEmail::message(Mailbox::new("ana@example.org"), "Hours?", "When do you open?"));
/// let page = outbox.read_inbox(None, 10).await.unwrap();
/// let thread = page.messages[0].thread.clone();
/// let fields = BTreeMap::from([("name".to_string(), "Ana".to_string())]);
/// outbox.reply(&thread, "hours", &fields).await.unwrap();
/// let sent = &mail.sent()[0];
/// assert_eq!(sent.to, [Mailbox::new("ana@example.org")]);
/// assert!(sent.text.ends_with("This message was written by an AI agent for Wanjiru's Bakery."));
/// # });
/// ```
pub struct EmailOutbox {
    driver: Arc<dyn EmailDriver>,
    from: Mailbox,
    disclosure: Disclosure,
    suppressions: Arc<dyn SuppressionList>,
    consent: Arc<dyn ConsentCheck>,
    auth: Option<(DomainAuthCheck, SenderPolicy)>,
    report: AsyncMutex<Option<DomainAuthReport>>,
    templates: ReplyTemplates,
    per_thread: Limiter,
    replies: Limiter,
    threads: Mutex<BTreeMap<String, KnownThread>>,
    unsubscribe_link: Option<UnsubscribeLink>,
}

impl std::fmt::Debug for EmailOutbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailOutbox")
            .field("driver", &self.driver.name())
            .field("from", &self.from)
            .finish_non_exhaustive()
    }
}

/// Builds an [`EmailOutbox`].
pub struct EmailOutboxBuilder {
    outbox: EmailOutbox,
}

impl EmailOutboxBuilder {
    /// Where unsubscribes and revocations are kept; in memory by default.
    #[must_use]
    pub fn suppressions(mut self, suppressions: Arc<dyn SuppressionList>) -> Self {
        self.outbox.suppressions = suppressions;
        self
    }

    /// Where consent receipts are checked; by default none is live, so no sequence or
    /// newsletter goes out.
    #[must_use]
    pub fn consent(mut self, consent: Arc<dyn ConsentCheck>) -> Self {
        self.outbox.consent = consent;
        self
    }

    /// How the sending domain's SPF, DKIM and DMARC are checked. Without it the domain is
    /// never checked, so sequences and newsletters are refused.
    #[must_use]
    pub fn domain_auth(mut self, lookup: Arc<dyn TxtLookup>, policy: SenderPolicy) -> Self {
        self.outbox.auth = Some((DomainAuthCheck::new(lookup), policy));
        self
    }

    /// The playbook's reply templates.
    #[must_use]
    pub fn templates(mut self, templates: ReplyTemplates) -> Self {
        self.outbox.templates = templates;
        self
    }

    /// The reply rate limits: per thread (3 an hour by default) and overall (60 an hour).
    #[must_use]
    pub fn reply_limits(mut self, per_thread: RateLimit, overall: RateLimit) -> Self {
        self.outbox.per_thread = Limiter::new(per_thread);
        self.outbox.replies = Limiter::new(overall);
        self
    }

    /// Makes each recipient's one-click unsubscribe link (RFC 8058), served by the edge;
    /// without it, sequences and newsletters offer the `mailto` unsubscribe only.
    #[must_use]
    pub fn unsubscribe_link(mut self, link: UnsubscribeLink) -> Self {
        self.outbox.unsubscribe_link = Some(link);
        self
    }

    /// The outbox.
    pub fn build(self) -> EmailOutbox {
        self.outbox
    }
}

impl EmailOutbox {
    /// An outbox sending through `driver` as `from`, every message ending with
    /// `disclosure`.
    pub fn builder(
        driver: Arc<dyn EmailDriver>,
        from: Mailbox,
        disclosure: Disclosure,
    ) -> EmailOutboxBuilder {
        EmailOutboxBuilder {
            outbox: Self {
                driver,
                from,
                disclosure,
                suppressions: Arc::new(InMemorySuppressions::new()),
                consent: Arc::new(InMemoryConsent::new()),
                auth: None,
                report: AsyncMutex::new(None),
                templates: ReplyTemplates::new(),
                per_thread: Limiter::new(RateLimit::new(3, Duration::from_secs(3_600))),
                replies: Limiter::new(RateLimit::new(60, Duration::from_secs(3_600))),
                threads: Mutex::default(),
                unsubscribe_link: None,
            },
        }
    }

    /// The driver.
    pub fn driver(&self) -> &Arc<dyn EmailDriver> {
        &self.driver
    }

    /// The suppression list.
    pub fn suppressions(&self) -> &Arc<dyn SuppressionList> {
        &self.suppressions
    }

    /// Reads the inbox after `cursor` (spec 3.5 §7.13, the public partition's read).
    ///
    /// Before the page is returned, an unsubscribe mail suppresses its sender for
    /// marketing, a bounce suppresses the bounced address, and a complaint suppresses the
    /// complainer for marketing (A-I17); every message's thread is remembered for
    /// `reply_in_thread`.
    ///
    /// # Errors
    ///
    /// Returns the driver's and the suppression list's errors.
    pub async fn read_inbox(&self, cursor: Option<&str>, limit: usize) -> Result<InboxPage> {
        let page = self.driver.inbox(cursor, limit).await?;
        for message in &page.messages {
            match (message.kind, &message.report_recipient) {
                (InboundKind::Unsubscribe, _) => {
                    self.suppressions
                        .suppress(Suppression::new(
                            &message.from.address,
                            SuppressionScope::Marketing,
                            "mailto",
                        ))
                        .await?;
                }
                (InboundKind::Bounce, Some(recipient)) => {
                    self.suppressions
                        .suppress(Suppression::new(recipient, SuppressionScope::All, "bounce"))
                        .await?;
                }
                (InboundKind::Complaint, Some(recipient)) => {
                    self.suppressions
                        .suppress(Suppression::new(
                            recipient,
                            SuppressionScope::Marketing,
                            "complaint",
                        ))
                        .await?;
                }
                _ => {}
            }
            if message.kind == InboundKind::Message {
                let mut references = message.references.clone();
                if !references.contains(&message.id) {
                    references.push(message.id.clone());
                }
                self.threads.lock().unwrap_or_else(PoisonError::into_inner).insert(
                    message.thread.clone(),
                    KnownThread {
                        sender: message.from.clone(),
                        subject: message.subject.clone(),
                        last_message: message.id.clone(),
                        references,
                        automatic: message.auto_submitted,
                    },
                );
            }
        }
        Ok(page)
    }

    /// Records an unsubscribe or revocation of `recipient` from `source`, such as the edge's
    /// one-click link (A-I17). It takes effect before the next send.
    ///
    /// # Errors
    ///
    /// Returns the suppression list's error.
    pub async fn unsubscribe(
        &self,
        recipient: &str,
        scope: SuppressionScope,
        source: &str,
    ) -> Result<()> {
        self.suppressions.suppress(Suppression::new(recipient, scope, source)).await
    }

    /// Checks the sending domain's SPF, DKIM and DMARC now, and keeps the report.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Refused`] when no domain check is configured, and the
    /// resolver's error.
    pub async fn check_domain(&self) -> Result<DomainAuthReport> {
        let Some((check, policy)) = &self.auth else {
            return Err(DriverError::refused(
                "no domain check is configured for this outbox; give it the sending domain's policy",
            ));
        };
        let report = check.run(policy).await?;
        *self.report.lock().await = Some(report.clone());
        Ok(report)
    }

    /// The last domain check, if one ran.
    pub async fn domain_report(&self) -> Option<DomainAuthReport> {
        self.report.lock().await.clone()
    }

    /// Runs the domain check before the first send, again when it is stale, and refuses
    /// bulk sending unless it passes.
    async fn before_send(&self, kind: SendKind) -> Result<()> {
        let report = self.report.lock().await.clone();
        let stale =
            report.as_ref().is_none_or(|report| Utc::now() - report.checked_at > AUTH_FRESHNESS);
        let report =
            if stale && self.auth.is_some() { Some(self.check_domain().await?) } else { report };
        if kind.is_bulk() {
            match report {
                Some(report) if report.passes() => {}
                Some(report) => {
                    return Err(DriverError::refused(format!(
                        "bulk sending waits until SPF, DKIM and DMARC pass for {}: {}",
                        report.domain,
                        report.failures().join("; ")
                    )));
                }
                None => {
                    return Err(DriverError::refused(
                        "bulk sending waits until the sending domain's SPF, DKIM and DMARC are checked and pass",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Refuses `recipient` when a suppression covers `kind`.
    async fn check_suppression(&self, recipient: &str, kind: SendKind) -> Result<()> {
        if let Some(suppression) = self.suppressions.lookup(recipient).await? {
            let covers = suppression.scope == SuppressionScope::All || kind != SendKind::Reply;
            if covers {
                return Err(DriverError::refused(format!(
                    "{recipient} unsubscribed or revoked ({:?}, {}); nothing more goes to them",
                    suppression.scope, suppression.source
                )));
            }
        }
        Ok(())
    }

    fn new_message_id(&self) -> String {
        let domain = self.from.address.rsplit('@').next().unwrap_or("site.invalid");
        format!("{}@{domain}", Uuid::new_v4().simple())
    }

    /// The unsubscribe headers and footer of a bulk message to `recipient`.
    fn unsubscribe_parts(&self, recipient: &str) -> (Vec<(String, String)>, String) {
        let mailto = format!("mailto:{}?subject=unsubscribe", self.from.address);
        let link = self.unsubscribe_link.as_ref().and_then(|link| link(recipient));
        let mut headers = Vec::new();
        let footer = match &link {
            Some(link) => {
                headers.push(("List-Unsubscribe".to_string(), format!("<{link}>, <{mailto}>")));
                headers.push((
                    "List-Unsubscribe-Post".to_string(),
                    "List-Unsubscribe=One-Click".to_string(),
                ));
                format!("Unsubscribe: {link}")
            }
            None => {
                headers.push(("List-Unsubscribe".to_string(), format!("<{mailto}>")));
                format!(
                    "Unsubscribe: reply with the subject \"unsubscribe\" or write to {}",
                    self.from.address
                )
            }
        };
        (headers, footer)
    }

    /// Answers thread `thread` with template `template` (spec 3.5 §7.10): to the thread's
    /// sender only, from the playbook, at the rate limits, ending with the disclosure.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Refused`] for a thread the inbox did not read, an automatic
    /// message, a revoked recipient or an unknown template; [`DriverError::RateLimited`]
    /// over a limit; and the driver's errors.
    pub async fn reply(
        &self,
        thread: &str,
        template: &str,
        fields: &BTreeMap<String, String>,
    ) -> Result<SentEmail> {
        let known = self
            .threads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(thread)
            .cloned()
            .ok_or_else(|| {
                DriverError::refused(
                    "reply_in_thread answers only a thread the inbox read; it never starts one",
                )
            })?;
        if known.automatic {
            return Err(DriverError::refused(
                "the thread's last message was sent automatically; it gets no reply",
            ));
        }
        let text = self.templates.render(template, fields)?;
        self.check_suppression(&known.sender.address, SendKind::Reply).await?;
        self.before_send(SendKind::Reply).await?;
        self.per_thread.take(thread, "replies in one thread")?;
        self.replies.take("all", "replies")?;
        let subject = if known.subject.to_ascii_lowercase().starts_with("re:") {
            known.subject.clone()
        } else {
            format!("Re: {}", known.subject)
        };
        let message = OutboundEmail {
            message_id: self.new_message_id(),
            from: self.from.clone(),
            to: vec![known.sender.clone()],
            subject,
            text: self.disclosure.apply(&text),
            in_reply_to: Some(known.last_message.clone()),
            references: known.references.clone(),
            headers: vec![("Auto-Submitted".into(), "auto-replied".into())],
        };
        self.driver.send(&message).await
    }

    /// Sends step `step` of sequence `sequence` to `to` (spec 3.5 §7.13, A-I8): bulk, so
    /// only once the domain passes, and only with a live consent receipt for
    /// `consent_scope`.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Refused`] for failing domain authentication, a missing
    /// consent receipt or a suppressed recipient, and the driver's errors.
    pub async fn send_sequence_step(
        &self,
        to: &Mailbox,
        subject: &str,
        text: &str,
        consent_scope: &str,
    ) -> Result<SentEmail> {
        self.before_send(SendKind::Sequence).await?;
        self.check_bulk_recipient(&to.address, consent_scope).await?;
        let (headers, footer) = self.unsubscribe_parts(&to.address);
        let message = OutboundEmail {
            message_id: self.new_message_id(),
            from: self.from.clone(),
            to: vec![to.clone()],
            subject: subject.to_string(),
            text: self.disclosure.apply(&format!("{}\n\n{footer}", text.trim_end())),
            in_reply_to: None,
            references: Vec::new(),
            headers,
        };
        self.driver.send(&message).await
    }

    async fn check_bulk_recipient(&self, address: &str, consent_scope: &str) -> Result<()> {
        self.check_suppression(address, SendKind::Sequence).await?;
        if !self.consent.is_live(address, consent_scope).await? {
            return Err(DriverError::refused(format!(
                "{address} holds no live consent for `{consent_scope}` (spec 3.5 A-I8)"
            )));
        }
        Ok(())
    }

    /// Sends a newsletter to each of `recipients` separately (spec 3.5 §7.13): bulk, so only
    /// once the domain passes; a recipient without a live consent receipt for
    /// `consent_scope`, or suppressed, is skipped and named in the report.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Refused`] for failing domain authentication; a recipient's
    /// failure is reported, not returned.
    pub async fn send_newsletter(
        &self,
        subject: &str,
        text: &str,
        recipients: &[Mailbox],
        consent_scope: &str,
    ) -> Result<NewsletterReport> {
        self.before_send(SendKind::Newsletter).await?;
        let mut report = NewsletterReport { sent: Vec::new(), skipped: Vec::new() };
        for recipient in recipients {
            if let Err(error) = self.check_bulk_recipient(&recipient.address, consent_scope).await {
                report.skipped.push((recipient.address.clone(), error.to_string()));
                continue;
            }
            let (headers, footer) = self.unsubscribe_parts(&recipient.address);
            let message = OutboundEmail {
                message_id: self.new_message_id(),
                from: self.from.clone(),
                to: vec![recipient.clone()],
                subject: subject.to_string(),
                text: self.disclosure.apply(&format!("{}\n\n{footer}", text.trim_end())),
                in_reply_to: None,
                references: Vec::new(),
                headers,
            };
            match self.driver.send(&message).await {
                Ok(_) => report.sent.push(recipient.address.clone()),
                Err(error) => report.skipped.push((recipient.address.clone(), error.to_string())),
            }
        }
        Ok(report)
    }
}

#[async_trait]
impl ThreadReplier for EmailOutbox {
    fn prefix(&self) -> &str {
        "email"
    }

    async fn reply_in_thread(
        &self,
        thread: &str,
        template: &str,
        fields: &BTreeMap<String, String>,
    ) -> Result<Value> {
        let sent = self.reply(thread, template, fields).await?;
        Ok(serde_json::to_value(sent).unwrap_or_default())
    }
}

/// The metrics of [`EmailOutbox`] as a [`KpiSource`].
pub const EMAIL_METRICS: [&str; 6] = [
    "email.sent",
    "email.rejected",
    "email.bounced",
    "email.complaints",
    "email.bounce_rate",
    "email.complaint_rate",
];

impl EmailOutbox {
    /// The metrics it measures, as KPIs name them.
    pub fn metrics(&self) -> Vec<String> {
        EMAIL_METRICS.iter().map(|metric| (*metric).to_string()).collect()
    }

    /// The value of `metric` over `[from, to)`; `None` when there is no data for the
    /// window.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for a metric it does
    /// not measure, and the service's errors.
    pub async fn measure(
        &self,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Option<f64>> {
        let stats = self.driver.delivery_stats(from, to).await?;
        let rate = |count: u64| (stats.sent > 0).then(|| count as f64 / stats.sent as f64);
        Ok(match metric {
            "email.sent" => Some(stats.sent as f64),
            "email.rejected" => Some(stats.rejected as f64),
            "email.bounced" => Some(stats.bounced as f64),
            "email.complaints" => Some(stats.complaints as f64),
            "email.bounce_rate" => rate(stats.bounced),
            "email.complaint_rate" => rate(stats.complaints),
            _ => return Err(unknown_metric("email", self.metrics(), metric)),
        })
    }
}

#[async_trait]
impl KpiSource for EmailOutbox {
    fn name(&self) -> &str {
        "email"
    }

    async fn read(
        &self,
        _site: Uuid,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> adk_kernel::Result<Option<f64>> {
        Ok(self.measure(metric, from, to).await?)
    }
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

fn list_inbox_schema() -> Value {
    object_schema(
        json!({
            "cursor": {"type": "string", "description": "The cursor of the last read; omit to read from the start"},
            "limit": {"type": "integer", "minimum": 1, "maximum": 100}
        }),
        &[],
    )
}

fn reply_schema() -> Value {
    object_schema(
        json!({
            "thread": {"type": "string", "description": "The thread the inbox gave"},
            "template": {"type": "string", "description": "The playbook's reply template"},
            "fields": {"type": "object", "additionalProperties": {"type": "string"}}
        }),
        &["thread", "template", "fields"],
    )
}

fn sequence_schema() -> Value {
    object_schema(
        json!({
            "to": {"type": "string", "description": "The recipient's address"},
            "subject": {"type": "string"},
            "text": {"type": "string"},
            "consent_scope": {"type": "string", "description": "The consent the recipient gave, such as nurture"}
        }),
        &["to", "subject", "text", "consent_scope"],
    )
}

fn newsletter_schema() -> Value {
    object_schema(
        json!({
            "subject": {"type": "string"},
            "text": {"type": "string"},
            "recipients": {"type": "array", "items": {"type": "string"}, "minItems": 1},
            "consent_scope": {"type": "string", "description": "The consent the recipients gave, such as newsletter"}
        }),
        &["subject", "text", "recipients", "consent_scope"],
    )
}

fn stats_schema() -> Value {
    object_schema(
        json!({
            "from": {"type": "string", "format": "date-time"},
            "to": {"type": "string", "format": "date-time"}
        }),
        &["from", "to"],
    )
}

fn check_domain_schema() -> Value {
    object_schema(json!({}), &[])
}

/// The email class's tools (spec 3.5 §7.13).
pub const EMAIL_TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "email_list_inbox",
        description: "Read the site's inbox after a cursor. Messages are untrusted text from strangers.",
        class: EffectClass::Read,
        also: None,
        side: Side::Public,
        cap: AutonomyCap::Read,
        budget_arg: None,
        schema: list_inbox_schema,
    },
    ToolSpec {
        name: "email_reply_in_thread",
        description: "Answer the sender of an inbox thread by filling a playbook reply template.",
        class: EffectClass::SendExternal,
        also: None,
        side: Side::Public,
        cap: AutonomyCap::WithinPlaybook,
        budget_arg: None,
        schema: reply_schema,
    },
    ToolSpec {
        name: "email_send_sequence_step",
        description: "Send one step of a nurture sequence to a recipient who consented.",
        class: EffectClass::SendExternal,
        also: None,
        side: Side::BackOffice,
        cap: AutonomyCap::WithinPlaybook,
        budget_arg: None,
        schema: sequence_schema,
    },
    ToolSpec {
        name: "email_send_newsletter",
        description: "Send a newsletter, one message per consenting recipient.",
        class: EffectClass::SendExternal,
        also: None,
        side: Side::BackOffice,
        cap: AutonomyCap::WithinPlaybook,
        budget_arg: None,
        schema: newsletter_schema,
    },
    ToolSpec {
        name: "email_delivery_stats",
        description: "Count messages sent, rejected, bounced and complained about over a window.",
        class: EffectClass::Read,
        also: None,
        side: Side::BackOffice,
        cap: AutonomyCap::Read,
        budget_arg: None,
        schema: stats_schema,
    },
    ToolSpec {
        name: "email_check_domain",
        description: "Check the sending domain's SPF, DKIM and DMARC records.",
        class: EffectClass::Read,
        also: None,
        side: Side::BackOffice,
        cap: AutonomyCap::Read,
        budget_arg: None,
        schema: check_domain_schema,
    },
];

/// The time argument `name`, as RFC 3339.
pub(crate) fn time_arg(args: &Value, name: &str) -> Result<DateTime<Utc>> {
    let text = str_arg(args, name)?;
    DateTime::parse_from_rfc3339(&text).map(|time| time.with_timezone(&Utc)).map_err(|_| {
        DriverError::invalid(format!("`{name}` is a time such as 2026-10-01T00:00:00Z"))
    })
}

/// The inbound message as a public agent reads it: its thread reference and its fields.
fn inbound_view(message: &InboundEmail) -> Value {
    json!({
        "thread": format!("email:{}", message.thread),
        "id": message.id,
        "from": message.from,
        "subject": message.subject,
        "text": message.text,
        "receivedAt": message.received_at,
        "kind": message.kind,
        "automatic": message.auto_submitted,
    })
}

/// The email tools of `side` over `outbox` (spec 3.5 §7.13).
pub fn email_tools(outbox: Arc<EmailOutbox>, side: Side) -> Vec<Arc<dyn Tool>> {
    let name = format!("email/{}", outbox.driver.name());
    let mut tools = Vec::new();
    for tool in EMAIL_TOOLS.iter().filter(|tool| tool.side == side) {
        let outbox = Arc::clone(&outbox);
        let tool: Arc<dyn Tool> = match tool.name {
            "email_list_inbox" => DriverTool::shared(tool, &name, move |args| {
                let outbox = Arc::clone(&outbox);
                Box::pin(async move {
                    let cursor = args.get("cursor").and_then(Value::as_str).map(str::to_string);
                    let limit =
                        args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 100)
                            as usize;
                    let page = outbox.read_inbox(cursor.as_deref(), limit).await?;
                    let messages: Vec<Value> = page.messages.iter().map(inbound_view).collect();
                    Ok(json!({ "messages": messages, "cursor": page.cursor }))
                })
            }),
            "email_reply_in_thread" => DriverTool::shared(tool, &name, move |args| {
                let outbox = Arc::clone(&outbox);
                Box::pin(async move {
                    let thread = str_arg(&args, "thread")?;
                    let thread = thread.strip_prefix("email:").unwrap_or(&thread).to_string();
                    let sent = outbox
                        .reply(&thread, &str_arg(&args, "template")?, &fields_arg(&args)?)
                        .await?;
                    Ok(serde_json::to_value(sent).unwrap_or_default())
                })
            }),
            "email_send_sequence_step" => DriverTool::shared(tool, &name, move |args| {
                let outbox = Arc::clone(&outbox);
                Box::pin(async move {
                    let to = Mailbox::new(str_arg(&args, "to")?);
                    let sent = outbox
                        .send_sequence_step(
                            &to,
                            &str_arg(&args, "subject")?,
                            &str_arg(&args, "text")?,
                            &str_arg(&args, "consent_scope")?,
                        )
                        .await?;
                    Ok(serde_json::to_value(sent).unwrap_or_default())
                })
            }),
            "email_send_newsletter" => DriverTool::shared(tool, &name, move |args| {
                let outbox = Arc::clone(&outbox);
                Box::pin(async move {
                    let recipients: Vec<Mailbox> = args
                        .get("recipients")
                        .and_then(Value::as_array)
                        .ok_or_else(|| DriverError::invalid("`recipients` is a list of addresses"))?
                        .iter()
                        .filter_map(Value::as_str)
                        .map(Mailbox::new)
                        .collect();
                    let report = outbox
                        .send_newsletter(
                            &str_arg(&args, "subject")?,
                            &str_arg(&args, "text")?,
                            &recipients,
                            &str_arg(&args, "consent_scope")?,
                        )
                        .await?;
                    Ok(serde_json::to_value(report).unwrap_or_default())
                })
            }),
            "email_delivery_stats" => DriverTool::shared(tool, &name, move |args| {
                let outbox = Arc::clone(&outbox);
                Box::pin(async move {
                    let stats = outbox
                        .driver
                        .delivery_stats(time_arg(&args, "from")?, time_arg(&args, "to")?)
                        .await?;
                    Ok(serde_json::to_value(stats).unwrap_or_default())
                })
            }),
            _ => DriverTool::shared(tool, &name, move |_args| {
                let outbox = Arc::clone(&outbox);
                Box::pin(async move {
                    Ok(serde_json::to_value(outbox.check_domain().await?).unwrap_or_default())
                })
            }),
        };
        tools.push(tool);
    }
    tools
}
