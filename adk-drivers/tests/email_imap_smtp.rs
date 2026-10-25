//! The IMAP and SMTP adapter against mail servers on loopback (spec 3.5 §7.13, §11): the
//! email suite in plain text on loopback and again under TLS — IMAPS from the first byte,
//! SMTP upgraded with STARTTLS — with a certificate the test makes.

#![cfg(feature = "imap-smtp")]

use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use adk_drivers::DriverError;
use adk_drivers::conformance::{EmailHarness, EmailServer, email_driver_contract};
use adk_drivers::email::{EmailDriver, ImapSmtpEmail, MailEndpoint, OutboundEmail};
use async_trait::async_trait;
use base64::Engine as _;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls;

const USER: &str = "hello@bakery.example";
const PASSWORD: &str = "correct horse \"battery\" staple";

/// What the two servers share: the mailbox and what was submitted.
#[derive(Default)]
struct MailState {
    mailbox: Vec<(u32, Vec<u8>)>,
    submitted: Vec<(Vec<String>, Vec<u8>)>,
    refused: Vec<String>,
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::server::TlsStream<TcpStream>>),
    Gone,
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_read(cx, buf),
            Self::Gone => Poll::Ready(Ok(())),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_write(cx, buf),
            Self::Gone => Poll::Ready(Ok(0)),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_flush(cx),
            Self::Gone => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_shutdown(cx),
            Self::Gone => Poll::Ready(Ok(())),
        }
    }
}

/// A server side connection read line by line.
struct Conn {
    stream: Stream,
    buffer: Vec<u8>,
}

impl Conn {
    async fn line(&mut self) -> Option<String> {
        loop {
            if let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buffer.drain(..=end).collect();
                return Some(
                    String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_string(),
                );
            }
            let mut chunk = [0u8; 4096];
            let read = self.stream.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    async fn send(&mut self, text: &str) {
        let _ = self.stream.write_all(text.as_bytes()).await;
        let _ = self.stream.flush().await;
    }

    async fn send_bytes(&mut self, bytes: &[u8]) {
        let _ = self.stream.write_all(bytes).await;
    }

    async fn upgrade(&mut self, acceptor: &TlsAcceptor) {
        let Stream::Plain(tcp) = std::mem::replace(&mut self.stream, Stream::Gone) else { return };
        if let Ok(tls) = acceptor.accept(tcp).await {
            self.stream = Stream::Tls(Box::new(tls));
        }
    }
}

/// How the fake servers secure their connections.
#[derive(Clone)]
struct Security {
    acceptor: Option<TlsAcceptor>,
    smtp_starttls: bool,
}

/// A loopback IMAP and SMTP pair over one shared mailbox.
struct FakeMail {
    state: Arc<Mutex<MailState>>,
    imap_port: u16,
    smtp_port: u16,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for FakeMail {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl FakeMail {
    async fn start(security: Security) -> Self {
        let state = Arc::new(Mutex::new(MailState::default()));
        let imap = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let smtp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (imap_port, smtp_port) =
            (imap.local_addr().unwrap().port(), smtp.local_addr().unwrap().port());
        let mut tasks = Vec::new();
        for (listener, is_imap) in [(imap, true), (smtp, false)] {
            let (state, security) = (Arc::clone(&state), security.clone());
            tasks.push(tokio::spawn(async move {
                loop {
                    let Ok((tcp, _)) = listener.accept().await else { return };
                    let (state, security) = (Arc::clone(&state), security.clone());
                    tokio::spawn(async move {
                        let mut conn = Conn { stream: Stream::Plain(tcp), buffer: Vec::new() };
                        if is_imap {
                            if let Some(acceptor) = &security.acceptor {
                                conn.upgrade(acceptor).await;
                            }
                            imap_session(conn, state).await;
                        } else {
                            smtp_session(conn, state, security).await;
                        }
                    });
                }
            }));
        }
        Self { state, imap_port, smtp_port, tasks }
    }
}

/// The words of an IMAP command, quoted strings unquoted.
fn imap_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' => {}
            '"' => {
                let mut word = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => word.extend(chars.next()),
                        '"' => break,
                        c => word.push(c),
                    }
                }
                words.push(word);
            }
            c => {
                let mut word = c.to_string();
                while let Some(c) = chars.peek().copied().filter(|c| *c != ' ') {
                    word.push(c);
                    chars.next();
                }
                words.push(word);
            }
        }
    }
    words
}

async fn imap_session(mut conn: Conn, state: Arc<Mutex<MailState>>) {
    conn.send("* OK [CAPABILITY IMAP4rev1] fake ready\r\n").await;
    let mut logged_in = false;
    while let Some(line) = conn.line().await {
        let words = imap_words(&line);
        let (Some(tag), Some(command)) = (words.first(), words.get(1)) else { continue };
        match (command.to_ascii_uppercase().as_str(), words.get(2).map(|w| w.to_ascii_uppercase()))
        {
            ("LOGIN", _) => {
                logged_in = words.get(2).map(String::as_str) == Some(USER)
                    && words.get(3).map(String::as_str) == Some(PASSWORD);
                let reply =
                    if logged_in { "OK logged in" } else { "NO [AUTHENTICATIONFAILED] wrong" };
                conn.send(&format!("{tag} {reply}\r\n")).await;
            }
            (_, _) if !logged_in => conn.send(&format!("{tag} BAD log in first\r\n")).await,
            ("EXAMINE", _) => {
                let count = state.lock().unwrap_or_else(PoisonError::into_inner).mailbox.len();
                conn.send(&format!(
                    "* {count} EXISTS\r\n* OK [UIDVALIDITY 777] ok\r\n{tag} OK [READ-ONLY] done\r\n"
                ))
                .await;
            }
            ("UID", Some(sub)) if sub == "SEARCH" => {
                let from: u32 = words
                    .get(4)
                    .and_then(|range| range.split(':').next())
                    .and_then(|first| first.parse().ok())
                    .unwrap_or(1);
                let uids: Vec<u32> = state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .mailbox
                    .iter()
                    .map(|(uid, _)| *uid)
                    .collect();
                let mut found: Vec<u32> = uids.iter().copied().filter(|uid| *uid >= from).collect();
                // `n:*` always includes the highest UID (RFC 9051 §6.4.4).
                if found.is_empty()
                    && let Some(last) = uids.last()
                {
                    found.push(*last);
                }
                let list: Vec<String> = found.iter().map(u32::to_string).collect();
                conn.send(&format!("* SEARCH {}\r\n{tag} OK done\r\n", list.join(" "))).await;
            }
            ("UID", Some(sub)) if sub == "FETCH" => {
                let wanted: Vec<u32> = words
                    .get(3)
                    .map(|set| set.split(',').filter_map(|uid| uid.parse().ok()).collect())
                    .unwrap_or_default();
                let messages = state.lock().unwrap_or_else(PoisonError::into_inner).mailbox.clone();
                for (sequence, (uid, raw)) in messages.iter().enumerate() {
                    if !wanted.contains(uid) {
                        continue;
                    }
                    conn.send(&format!(
                        "* {} FETCH (UID {uid} INTERNALDATE \"19-Oct-2026 08:00:00 +0000\" BODY[] {{{}}}\r\n",
                        sequence + 1,
                        raw.len()
                    ))
                    .await;
                    conn.send_bytes(raw).await;
                    conn.send(")\r\n").await;
                }
                conn.send(&format!("{tag} OK done\r\n")).await;
            }
            ("LOGOUT", _) => {
                conn.send(&format!("* BYE\r\n{tag} OK bye\r\n")).await;
                return;
            }
            _ => conn.send(&format!("{tag} BAD unknown\r\n")).await,
        }
    }
}

async fn smtp_session(mut conn: Conn, state: Arc<Mutex<MailState>>, security: Security) {
    conn.send("220 fake ESMTP\r\n").await;
    let (mut authenticated, mut recipients) = (false, Vec::new());
    while let Some(line) = conn.line().await {
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("EHLO") {
            let tls = if security.smtp_starttls && matches!(conn.stream, Stream::Plain(_)) {
                "250-STARTTLS\r\n"
            } else {
                ""
            };
            conn.send(&format!("250-fake\r\n{tls}250-AUTH PLAIN LOGIN\r\n250 8BITMIME\r\n")).await;
        } else if upper == "STARTTLS" {
            conn.send("220 go ahead\r\n").await;
            if let Some(acceptor) = &security.acceptor {
                conn.upgrade(acceptor).await;
            }
        } else if let Some(token) = line.strip_prefix("AUTH PLAIN ") {
            let decoded =
                base64::engine::general_purpose::STANDARD.decode(token.trim()).unwrap_or_default();
            authenticated = decoded == format!("\0{USER}\0{PASSWORD}").into_bytes();
            conn.send(if authenticated { "235 ok\r\n" } else { "535 wrong\r\n" }).await;
        } else if !authenticated {
            conn.send("530 authenticate first\r\n").await;
        } else if upper.starts_with("MAIL FROM:") {
            recipients.clear();
            conn.send("250 ok\r\n").await;
        } else if upper.starts_with("RCPT TO:") {
            let address = line[8..].trim().trim_matches(['<', '>']).to_ascii_lowercase();
            if state.lock().unwrap_or_else(PoisonError::into_inner).refused.contains(&address) {
                conn.send("550 no such user\r\n").await;
            } else {
                recipients.push(address);
                conn.send("250 ok\r\n").await;
            }
        } else if upper == "DATA" {
            conn.send("354 go\r\n").await;
            let mut data = Vec::new();
            while let Some(line) = conn.line().await {
                if line == "." {
                    break;
                }
                let line = line.strip_prefix('.').map(str::to_string).unwrap_or(line);
                data.extend_from_slice(line.as_bytes());
                data.extend_from_slice(b"\r\n");
            }
            state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .submitted
                .push((std::mem::take(&mut recipients), data));
            conn.send("250 queued\r\n").await;
        } else if upper == "RSET" {
            recipients.clear();
            conn.send("250 ok\r\n").await;
        } else if upper == "QUIT" {
            conn.send("221 bye\r\n").await;
            return;
        } else {
            conn.send("500 unknown\r\n").await;
        }
    }
}

struct Served {
    driver: ImapSmtpEmail,
    _mail: Arc<FakeMail>,
}

#[async_trait]
impl EmailDriver for Served {
    fn name(&self) -> &str {
        self.driver.name()
    }

    async fn inbox(
        &self,
        cursor: Option<&str>,
        limit: usize,
    ) -> adk_drivers::Result<adk_drivers::email::InboxPage> {
        self.driver.inbox(cursor, limit).await
    }

    async fn send(
        &self,
        message: &OutboundEmail,
    ) -> adk_drivers::Result<adk_drivers::email::SentEmail> {
        self.driver.send(message).await
    }

    async fn delivery_stats(
        &self,
        from: chrono::DateTime<chrono::Utc>,
        to: chrono::DateTime<chrono::Utc>,
    ) -> adk_drivers::Result<adk_drivers::email::DeliveryStats> {
        self.driver.delivery_stats(from, to).await
    }
}

struct Server(Arc<FakeMail>);

#[async_trait]
impl EmailServer for Server {
    async fn deliver(&self, raw: &[u8]) {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        let uid = state.mailbox.last().map_or(100, |(uid, _)| uid + 3);
        state.mailbox.push((uid, raw.to_vec()));
    }

    async fn sent(&self) -> Vec<(Vec<String>, Vec<u8>)> {
        self.0.state.lock().unwrap_or_else(PoisonError::into_inner).submitted.clone()
    }

    async fn refuse(&self, address: &str) {
        self.0
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .refused
            .push(address.to_ascii_lowercase());
    }
}

/// A certificate for `localhost` and the acceptor that serves it.
fn tls() -> (Vec<u8>, TlsAcceptor) {
    adk_core::ensure_crypto_provider();
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let der = certified.cert.der().to_vec();
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(certified.signing_key.serialize_der().into());
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![rustls::pki_types::CertificateDer::from(der.clone())], key)
        .unwrap();
    (der, TlsAcceptor::from(Arc::new(config)))
}

struct Plain;

#[async_trait]
impl EmailHarness for Plain {
    async fn start(&self) -> (Arc<dyn EmailDriver>, Arc<dyn EmailServer>) {
        let mail =
            Arc::new(FakeMail::start(Security { acceptor: None, smtp_starttls: false }).await);
        let driver = ImapSmtpEmail::new(
            MailEndpoint::plain("127.0.0.1", mail.imap_port).unwrap(),
            MailEndpoint::plain("127.0.0.1", mail.smtp_port).unwrap(),
            USER,
            PASSWORD,
        )
        .unwrap();
        (Arc::new(Served { driver, _mail: Arc::clone(&mail) }), Arc::new(Server(mail)))
    }
}

struct Tls;

#[async_trait]
impl EmailHarness for Tls {
    async fn start(&self) -> (Arc<dyn EmailDriver>, Arc<dyn EmailServer>) {
        let (der, acceptor) = tls();
        let mail = Arc::new(
            FakeMail::start(Security { acceptor: Some(acceptor), smtp_starttls: true }).await,
        );
        let driver = ImapSmtpEmail::new(
            MailEndpoint::tls("localhost", mail.imap_port).unwrap(),
            MailEndpoint::starttls("localhost", mail.smtp_port).unwrap(),
            USER,
            PASSWORD,
        )
        .unwrap()
        .with_root_certificate(der)
        .unwrap();
        (Arc::new(Served { driver, _mail: Arc::clone(&mail) }), Arc::new(Server(mail)))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imap_and_smtp_on_loopback_keep_the_email_contract() {
    email_driver_contract(&Plain).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imaps_and_smtp_starttls_keep_the_email_contract() {
    email_driver_contract(&Tls).await;
}

fn message() -> OutboundEmail {
    serde_json::from_value(serde_json::json!({
        "messageId": "t1@bakery.example",
        "from": {"address": USER},
        "to": [{"address": "ana@example.org"}],
        "subject": "Hello",
        "text": "Hi"
    }))
    .unwrap()
}

#[tokio::test]
async fn a_wrong_password_is_unauthorized_and_never_echoed() {
    let mail = FakeMail::start(Security { acceptor: None, smtp_starttls: false }).await;
    let driver = ImapSmtpEmail::new(
        MailEndpoint::plain("127.0.0.1", mail.imap_port).unwrap(),
        MailEndpoint::plain("127.0.0.1", mail.smtp_port).unwrap(),
        USER,
        "not-the-password",
    )
    .unwrap();
    let error = driver.inbox(None, 10).await.expect_err("IMAP refuses the password");
    assert!(matches!(error, DriverError::Unauthorized { .. }), "{error:?}");
    let error = driver.send(&message()).await.expect_err("SMTP refuses the password");
    assert!(matches!(error, DriverError::Unauthorized { .. }), "{error:?}");
    assert!(
        !error.to_string().contains("not-the-password")
            && !format!("{driver:?}").contains("not-the-password")
    );
}

#[tokio::test]
async fn a_server_without_starttls_gets_nothing_in_clear_text() {
    let (der, _) = tls();
    let mail = FakeMail::start(Security { acceptor: None, smtp_starttls: false }).await;
    let driver = ImapSmtpEmail::new(
        MailEndpoint::plain("127.0.0.1", mail.imap_port).unwrap(),
        MailEndpoint::starttls("localhost", mail.smtp_port).unwrap(),
        USER,
        PASSWORD,
    )
    .unwrap()
    .with_root_certificate(der)
    .unwrap();
    let error = driver.send(&message()).await.expect_err("no STARTTLS, no send");
    assert!(matches!(error, DriverError::Refused { .. }), "{error:?}");
    assert!(mail.state.lock().unwrap().submitted.is_empty());
    assert!(MailEndpoint::plain("mail.example.com", 25).is_err(), "clear text only on loopback");
}
