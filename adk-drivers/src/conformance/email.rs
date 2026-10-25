//! The email suite: the driver contract and the outbox rules over it (spec 3.5 §7.10,
//! §7.13, A-I8, A-I17, §11).

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};

use crate::email::{
    EmailDriver, EmailOutbox, InMemoryEmail, InboundEmail, InboundKind, Mailbox, OutboundEmail,
    SenderPolicy, StaticTxt,
};
use crate::error::DriverError;
use crate::outbound::{Disclosure, InMemoryConsent, ReplyTemplates, SuppressionScope};

/// The test side of an email service: what a person or a remote server does to it.
#[async_trait]
pub trait EmailServer: Send + Sync {
    /// A raw RFC 5322 message arrives in the inbox.
    async fn deliver(&self, raw: &[u8]);

    /// What left the machine: each message's recipients and its wire form.
    async fn sent(&self) -> Vec<(Vec<String>, Vec<u8>)>;

    /// The server refuses `address` as a recipient from now on.
    async fn refuse(&self, address: &str);
}

/// Makes a fresh adapter and its server for [`email_driver_contract`].
#[async_trait]
pub trait EmailHarness: Send + Sync {
    /// A fresh adapter over an empty mailbox, and the server it talks to.
    async fn start(&self) -> (Arc<dyn EmailDriver>, Arc<dyn EmailServer>);
}

/// The suite's harness over [`InMemoryEmail`].
#[derive(Debug, Default, Clone, Copy)]
pub struct InMemoryEmailHarness;

struct InMemoryServer(Arc<InMemoryEmail>);

#[async_trait]
impl EmailServer for InMemoryServer {
    async fn deliver(&self, raw: &[u8]) {
        self.0.deliver(InboundEmail::parse(raw, Utc::now()));
    }

    async fn sent(&self) -> Vec<(Vec<String>, Vec<u8>)> {
        self.0
            .sent()
            .into_iter()
            .map(|message| {
                let to = message.to.iter().map(|mailbox| mailbox.address.clone()).collect();
                (to, message.to_wire(Utc::now()).expect("a sent message renders"))
            })
            .collect()
    }

    async fn refuse(&self, address: &str) {
        self.0.refuse(address);
    }
}

#[async_trait]
impl EmailHarness for InMemoryEmailHarness {
    async fn start(&self) -> (Arc<dyn EmailDriver>, Arc<dyn EmailServer>) {
        let email = Arc::new(InMemoryEmail::new());
        (email.clone(), Arc::new(InMemoryServer(email)))
    }
}

const SITE: &str = "hello@bakery.example";
const OWNER: &str = "Wanjiru's Bakery";

fn raw(from: &str, id: &str, subject: &str, body: &str, extra: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {SITE}\r\nSubject: {subject}\r\nMessage-ID: <{id}>\r\n{extra}Date: Mon, 19 Oct 2026 08:00:00 +0000\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

fn dsn(recipient: &str) -> Vec<u8> {
    format!(
        "From: mailer-daemon@mx.example\r\nTo: {SITE}\r\nSubject: Undelivered\r\nMessage-ID: <dsn-1@mx.example>\r\n\
Content-Type: multipart/report; report-type=delivery-status; boundary=r\r\n\r\n--r\r\nContent-Type: text/plain\r\n\r\n\
Delivery failed\r\n--r\r\nContent-Type: message/delivery-status\r\n\r\nReporting-MTA: dns; mx.example\r\n\r\n\
Final-Recipient: rfc822; {recipient}\r\nAction: failed\r\nStatus: 5.1.1\r\n--r--\r\n"
    )
    .into_bytes()
}

fn passing_txt() -> StaticTxt {
    StaticTxt::new()
        .with("bakery.example", "v=spf1 include:_spf.mail.example ~all")
        .with(
            "mail._domainkey.bakery.example",
            "v=DKIM1; k=rsa; p=MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8A",
        )
        .with("_dmarc.bakery.example", "v=DMARC1; p=quarantine")
}

fn outbox(
    driver: Arc<dyn EmailDriver>,
    txt: Arc<StaticTxt>,
    consent: Arc<InMemoryConsent>,
) -> EmailOutbox {
    EmailOutbox::builder(
        driver,
        Mailbox::new(SITE).with_name("Wanjiru's Bakery"),
        Disclosure::new(OWNER).unwrap(),
    )
    .domain_auth(txt, SenderPolicy::new("bakery.example", "mail", "include:_spf.mail.example"))
    .consent(consent)
    .templates(ReplyTemplates::new().template("hours", "Hi {name}, we open at {time}."))
    .build()
}

fn fields() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("name".to_string(), "Ana".to_string()),
        ("time".to_string(), "7am".to_string()),
    ])
}

fn parsed(wire: &[u8]) -> InboundEmail {
    InboundEmail::parse(wire, Utc::now())
}

fn header(wire: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(wire);
    let head = text.split("\r\n\r\n").next().unwrap_or_default();
    head.split("\r\n").find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim().to_string())
    })
}

/// Runs every email case (spec 3.5 §7.13, §7.10, A-I8, A-I17).
///
/// | Part | Cases |
/// |---|---|
/// | Driver | The inbox reads in order after a cursor and pages by its limit; a sent message keeps its recipients, subject, text and threading on the wire; a refused recipient is reported and counted; a bounce in the inbox is counted |
/// | Outbox | A reply goes to the thread's sender only and ends with the disclosure; a reply needs a thread the inbox read and never answers an automatic message; a revocation stops the next reply; bulk waits for SPF, DKIM and DMARC; bulk needs live consent; an unsubscribe mail stops the next send; a newsletter skips who may not receive it |
///
/// # Panics
///
/// When the adapter breaks the contract, naming the case.
pub async fn email_driver_contract(harness: &dyn EmailHarness) {
    driver_cases(harness).await;
    outbox_cases(harness).await;
}

async fn driver_cases(harness: &dyn EmailHarness) {
    // The inbox reads in order after a cursor.
    let (email, server) = harness.start().await;
    server
        .deliver(&raw("Ana <ana@example.org>", "m1@example.org", "Hours?", "When do you open?", ""))
        .await;
    server
        .deliver(&raw("bob@example.org", "m2@example.org", "Parking", "Is there parking?", ""))
        .await;
    let page = email.inbox(None, 10).await.expect("inbox: reads");
    let subjects: Vec<&str> = page.messages.iter().map(|m| m.subject.as_str()).collect();
    assert_eq!(subjects, ["Hours?", "Parking"], "inbox: oldest first");
    assert_eq!(
        page.messages[0].from,
        Mailbox::new("ana@example.org").with_name("Ana"),
        "inbox: the sender"
    );
    assert_eq!(page.messages[0].text, "When do you open?", "inbox: the text");
    let empty =
        email.inbox(page.cursor.as_deref(), 10).await.expect("inbox: reads after the cursor");
    assert!(empty.messages.is_empty(), "inbox: nothing after the cursor");
    server
        .deliver(&raw("cy@example.org", "m3@example.org", "Cakes", "Do you bake cakes?", ""))
        .await;
    let next = email.inbox(empty.cursor.as_deref(), 10).await.expect("inbox: reads the next");
    assert_eq!(next.messages.len(), 1, "inbox: only the new message");
    assert_eq!(next.messages[0].id, "m3@example.org", "inbox: the new message");

    // A page is limited.
    let limited = email.inbox(None, 2).await.expect("limit: reads");
    assert_eq!(limited.messages.len(), 2, "limit: two of three");
    let rest = email.inbox(limited.cursor.as_deref(), 2).await.expect("limit: reads the rest");
    assert_eq!(rest.messages.len(), 1, "limit: the third");

    // A sent message keeps its fields on the wire.
    let (email, server) = harness.start().await;
    let message = OutboundEmail {
        message_id: "out-1@bakery.example".into(),
        from: Mailbox::new(SITE),
        to: vec![Mailbox::new("ana@example.org")],
        subject: "Re: Café hours".into(),
        text: "We open at 7am.\n\n— the bakery".into(),
        in_reply_to: Some("m1@example.org".into()),
        references: vec!["m1@example.org".into()],
        headers: Vec::new(),
    };
    let sent = email.send(&message).await.expect("send: accepted");
    assert_eq!(sent.accepted, ["ana@example.org"], "send: the recipient accepted");
    let wire = server.sent().await;
    assert_eq!(wire.len(), 1, "send: one message left");
    assert_eq!(wire[0].0, ["ana@example.org"], "send: to the recipient only");
    let back = parsed(&wire[0].1);
    assert_eq!(
        (back.subject.as_str(), back.text.as_str()),
        ("Re: Café hours", message.text.as_str()),
        "send: subject and text"
    );
    assert_eq!(back.in_reply_to.as_deref(), Some("m1@example.org"), "send: threading");

    // A refused recipient is reported and counted.
    let (email, server) = harness.start().await;
    server.refuse("gone@example.org").await;
    let start = Utc::now() - Duration::minutes(1);
    let mut both = message.clone();
    both.to = vec![Mailbox::new("ana@example.org"), Mailbox::new("gone@example.org")];
    let sent = email.send(&both).await.expect("refused: the other recipient is accepted");
    assert_eq!(sent.accepted, ["ana@example.org"], "refused: only the accepted recipient");
    let mut only = message.clone();
    only.to = vec![Mailbox::new("gone@example.org")];
    let error = email.send(&only).await.expect_err("refused: no recipient accepted");
    assert!(matches!(error, DriverError::Refused { .. }), "refused: as refused: {error:?}");
    let stats = email
        .delivery_stats(start, Utc::now() + Duration::minutes(1))
        .await
        .expect("refused: stats");
    assert_eq!(stats.sent, 1, "refused: one message sent");
    assert!(stats.rejected >= 2, "refused: both refusals counted: {stats:?}");

    // A bounce in the inbox is counted.
    let (email, server) = harness.start().await;
    server.deliver(&dsn("gone@example.org")).await;
    let page = email.inbox(None, 10).await.expect("bounce: reads");
    assert_eq!(page.messages[0].kind, InboundKind::Bounce, "bounce: read as a bounce");
    let stats = email
        .delivery_stats(Utc::now() - Duration::days(30_000), Utc::now() + Duration::days(1))
        .await
        .expect("bounce: stats");
    assert_eq!(stats.bounced, 1, "bounce: counted");
}

async fn outbox_cases(harness: &dyn EmailHarness) {
    let disclosure = Disclosure::new(OWNER).unwrap();

    // A reply goes to the thread's sender only and ends with the disclosure.
    let (email, server) = harness.start().await;
    let rules = outbox(email, Arc::new(passing_txt()), Arc::new(InMemoryConsent::new()));
    server
        .deliver(&raw(
            "Ana <ana@example.org>",
            "q1@example.org",
            "Hours?",
            "When?",
            "Cc: bob@example.org\r\n",
        ))
        .await;
    let page = rules.read_inbox(None, 10).await.expect("reply: reads");
    let thread = page.messages[0].thread.clone();
    rules.reply(&thread, "hours", &fields()).await.expect("reply: sent");
    let wire = server.sent().await;
    assert_eq!(wire.len(), 1, "reply: one message");
    assert_eq!(wire[0].0, ["ana@example.org"], "reply: to the sender only, no new recipients");
    let back = parsed(&wire[0].1);
    assert!(disclosure.is_present(&back.text), "reply: ends with the disclosure: {:?}", back.text);
    assert!(back.text.starts_with("Hi Ana, we open at 7am."), "reply: the template filled");
    assert_eq!(back.in_reply_to.as_deref(), Some("q1@example.org"), "reply: in the thread");
    assert_eq!(back.subject, "Re: Hours?", "reply: the thread's subject");

    // A reply needs a thread the inbox read, and never answers an automatic message.
    let error = rules
        .reply("never-read@example.org", "hours", &fields())
        .await
        .expect_err("reply: unknown thread");
    assert!(
        matches!(error, DriverError::Refused { .. }),
        "reply: unknown thread refused: {error:?}"
    );
    server
        .deliver(&raw(
            "auto@example.org",
            "auto-1@example.org",
            "Out of office",
            "Away",
            "Auto-Submitted: auto-replied\r\n",
        ))
        .await;
    let page =
        rules.read_inbox(page.cursor.as_deref(), 10).await.expect("reply: reads the auto-reply");
    let error = rules
        .reply(&page.messages[0].thread, "hours", &fields())
        .await
        .expect_err("reply: automatic");
    assert!(
        matches!(error, DriverError::Refused { .. }),
        "reply: an automatic message gets no reply: {error:?}"
    );

    // A revocation stops the next reply.
    rules
        .unsubscribe("ANA@example.org", SuppressionScope::All, "owner")
        .await
        .expect("revoke: recorded");
    let error = rules
        .reply(&thread, "hours", &fields())
        .await
        .expect_err("revoke: the next reply is refused");
    assert!(matches!(error, DriverError::Refused { .. }), "revoke: refused: {error:?}");
    assert_eq!(server.sent().await.len(), 1, "revoke: nothing more left");

    // Bulk waits for SPF, DKIM and DMARC.
    let (email, server) = harness.start().await;
    let txt =
        Arc::new(StaticTxt::new().with("bakery.example", "v=spf1 include:_spf.mail.example ~all"));
    let consent = Arc::new(InMemoryConsent::new());
    consent.grant("ana@example.org", "nurture");
    let rules = outbox(email, txt.clone(), consent.clone());
    let ana = Mailbox::new("ana@example.org");
    let error = rules
        .send_sequence_step(&ana, "Fresh bread", "Our sourdough is back.", "nurture")
        .await
        .expect_err("bulk: refused while DKIM and DMARC are missing");
    assert!(matches!(error, DriverError::Refused { .. }), "bulk: refused: {error:?}");
    let report = rules.domain_report().await.expect("bulk: the check ran before the first send");
    assert!(
        report.spf.pass && !report.dkim.pass && !report.dmarc.pass,
        "bulk: the report says why: {report:?}"
    );
    assert!(server.sent().await.is_empty(), "bulk: nothing left");
    txt.insert(
        "mail._domainkey.bakery.example",
        "v=DKIM1; k=rsa; p=MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8A",
    );
    txt.insert("_dmarc.bakery.example", "v=DMARC1; p=none");
    assert!(
        rules.check_domain().await.expect("bulk: checks again").passes(),
        "bulk: passes once published"
    );
    rules
        .send_sequence_step(&ana, "Fresh bread", "Our sourdough is back.", "nurture")
        .await
        .expect("bulk: sent");
    let wire = server.sent().await;
    assert_eq!(wire.len(), 1, "bulk: one message");
    assert!(
        header(&wire[0].1, "List-Unsubscribe").is_some_and(|value| value.contains("mailto:")),
        "bulk: unsubscribe header"
    );
    let back = parsed(&wire[0].1);
    assert!(disclosure.is_present(&back.text), "bulk: ends with the disclosure: {:?}", back.text);
    assert!(back.text.contains("Unsubscribe"), "bulk: says how to unsubscribe");

    // Bulk needs live consent, checked at send time.
    let bob = Mailbox::new("bob@example.org");
    let error = rules
        .send_sequence_step(&bob, "Hi", "Hello", "nurture")
        .await
        .expect_err("consent: none given");
    assert!(matches!(error, DriverError::Refused { .. }), "consent: refused: {error:?}");
    consent.grant("bob@example.org", "nurture");
    consent.revoke("bob@example.org", "nurture");
    assert!(
        rules.send_sequence_step(&bob, "Hi", "Hello", "nurture").await.is_err(),
        "consent: a revoked receipt refuses"
    );

    // An unsubscribe mail stops the next send.
    server.deliver(&raw("ana@example.org", "unsub-1@example.org", "unsubscribe", "", "")).await;
    rules.read_inbox(None, 10).await.expect("unsubscribe: reads");
    let error = rules
        .send_sequence_step(&ana, "Fresh bread", "Again", "nurture")
        .await
        .expect_err("unsubscribe: the next send is refused");
    assert!(matches!(error, DriverError::Refused { .. }), "unsubscribe: refused: {error:?}");
    assert_eq!(server.sent().await.len(), 1, "unsubscribe: nothing more left");

    // A newsletter skips who may not receive it, one message per recipient.
    consent.grant("cy@example.org", "newsletter");
    consent.grant("ana@example.org", "newsletter");
    let recipients = [Mailbox::new("cy@example.org"), Mailbox::new("dee@example.org"), ana.clone()];
    let report = rules
        .send_newsletter("October", "New loaves this month.", &recipients, "newsletter")
        .await
        .expect("newsletter: sends");
    assert_eq!(
        report.sent,
        ["cy@example.org"],
        "newsletter: only the consenting, subscribed recipient"
    );
    assert_eq!(report.skipped.len(), 2, "newsletter: two skipped with their reasons");
    let wire = server.sent().await;
    assert_eq!(wire.len(), 2, "newsletter: one more message");
    assert_eq!(wire[1].0, ["cy@example.org"], "newsletter: addressed to one recipient");
}
