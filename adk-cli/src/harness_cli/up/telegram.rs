//! The Telegram chat thread of `adk-rust up` (spec 3.3 §5.3), started when
//! `TELEGRAM_BOT_TOKEN` is set; without it nothing of it is built, started or logged.
//!
//! The bot's updates are read by long polling, so the kernel keeps its loopback bind. A
//! paired account's messages are turns on the harness; each decision rings its thread; the
//! account is paired with `adk-rust pair accept <code>` and listed in the home under "Where
//! you can say yes from", in the store the kernel's other pairings are in.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use adk_core::ApprovalService;
use adk_core::audit_log::{AuditRecordDraft, AuditRecorder, AuditTrail};
use adk_harness::HarnessMode;
use adk_kernel::channels::{ChannelBindingStore, ChannelDoorbell, ChannelGateway, ChannelHost};
use adk_surface::AnswerLog;
use adk_surface::channels::TelegramAdapter;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;

use super::super::{APP_NAME, Setup};

/// The bot's token, from `@BotFather`.
const TOKEN_VAR: &str = "TELEGRAM_BOT_TOKEN";
/// Another Bot API address, such as a local Bot API server; `https://api.telegram.org` when
/// unset.
const API_VAR: &str = "TELEGRAM_API_BASE";
/// The home's address as the person's phone opens it, which cards and notices link to; the
/// bind address when unset.
const HOME_VAR: &str = "TELEGRAM_HOME_URL";

/// The chat thread before the kernel is built: the doorbell the approval service rings, and
/// where paired accounts are kept.
pub(super) struct Chat {
    token: String,
    api_base: Option<String>,
    home: Option<String>,
    doorbell: Arc<ChannelDoorbell>,
    bindings: Arc<dyn ChannelBindingStore>,
    /// A feature's chat headings, as the home's engine has them (spec 3.3 §7.1).
    card_hints: Vec<Arc<dyn adk_surface::CardHints>>,
}

impl Chat {
    /// The chat thread when `TELEGRAM_BOT_TOKEN` is set, its accounts kept beside the
    /// kernel's other stores; `None` otherwise.
    ///
    /// # Errors
    ///
    /// Refuses `--mode auto` without `--sandbox`, since a paired account's messages come
    /// from outside the machine as a non-loopback bind's requests do (spec 3.3 S-I6), and a
    /// malformed token, API address or home address.
    pub(super) async fn from_env(setup: &Setup, store: Option<&str>) -> Result<Option<Self>> {
        let Some(token) = env(TOKEN_VAR) else { return Ok(None) };
        refuse_unattended(setup.resolved.mode, setup.resolved.sandbox)?;
        let api_base = env(API_VAR);
        // A malformed token or address stops `up` before anything is opened.
        adapter(&token, api_base.as_deref())
            .build()
            .with_context(|| format!("{TOKEN_VAR} or {API_VAR} cannot be used"))?;
        let home = env(HOME_VAR);
        if let Some(home) = &home {
            web_address(home)?;
        }
        Ok(Some(Self {
            token,
            api_base,
            home,
            doorbell: Arc::new(ChannelDoorbell::new()),
            bindings: bindings(setup, store).await?,
            card_hints: Vec::new(),
        }))
    }

    /// Heads cards with `hints`' chat headings, as the home's engine does (spec 3.3 §7.1).
    #[cfg(feature = "verb-ui")]
    pub(super) fn with_card_hints(mut self, hints: Arc<dyn adk_surface::CardHints>) -> Self {
        self.card_hints.push(hints);
        self
    }

    /// The doorbell to register on the approval service.
    pub(super) fn doorbell(&self) -> Arc<ChannelDoorbell> {
        self.doorbell.clone()
    }

    /// Where paired accounts are kept, for the control plane's pairing.
    pub(super) fn bindings(&self) -> Arc<dyn ChannelBindingStore> {
        self.bindings.clone()
    }

    /// Starts the gateway over `host`, the kernel, and the bot's polling. Cards read their
    /// decisions from `approvals` and link to the home ([`home_url`]); each answer given in
    /// Telegram is recorded on `records`, the trail and answer log the home's engine has, so
    /// the home says "You said yes in Telegram". Returns once the gateway runs: the cards of
    /// decisions that waited through a restart are sent in the background, so Telegram never
    /// holds up the home.
    /// Links cards to `home` unless `TELEGRAM_HOME_URL` names another address.
    #[cfg(feature = "mobile")]
    pub(super) fn linking_to(mut self, home: String) -> Self {
        if self.home.is_none() {
            self.home = Some(home);
        }
        self
    }

    pub(super) async fn start(
        self,
        host: Arc<dyn ChannelHost>,
        approvals: Arc<dyn ApprovalService>,
        records: Option<(AuditTrail, Arc<dyn AnswerLog>)>,
        bind: SocketAddr,
    ) -> Result<()> {
        let mut adapter = adapter(&self.token, self.api_base.as_deref()).approvals(approvals);
        for hints in self.card_hints {
            adapter = adapter.card_hints(hints);
        }
        if let Some((trail, answers)) = records {
            adapter = adapter.audit(trail).answer_log(answers);
        }
        let adapter = adapter.build()?;
        let home = home_url(bind, self.home.as_deref());
        let gateway = ChannelGateway::builder(host, self.bindings)
            .sink(Arc::new(adapter.clone()))
            .target(APP_NAME)
            .home_url(home.clone())
            .doorbell(self.doorbell)
            .build();
        gateway.start().await?;
        let polling = adapter.start(gateway.clone());
        tokio::spawn(async move {
            // Decisions that waited through a restart reach the thread again.
            if let Err(error) = gateway.ring_waiting().await {
                tracing::warn!(error = %error, "the waiting decisions could not be sent to telegram");
            }
            if let Err(error) = polling.join().await {
                tracing::error!(error = %error, "the telegram bot stopped reading messages; restart adk-rust up with a working TELEGRAM_BOT_TOKEN");
            }
            // The gateway lives as long as `up`, so the decisions the approval service rings
            // keep being taken from its doorbell instead of piling up there.
            std::future::pending::<()>().await;
            drop(gateway);
        });
        println!(
            "  chat  Telegram: an account that writes to the bot is sent a code; pair it with \
             `adk-rust pair accept <code>`; cards link to {home}"
        );
        Ok(())
    }
}

/// Refuses Go ahead without a sandbox: a paired account's messages would run edits
/// unattended on this host, which `up` refuses beyond loopback for the same reason.
fn refuse_unattended(mode: HarnessMode, sandbox: bool) -> Result<()> {
    if mode == HarnessMode::Auto && !sandbox {
        bail!(
            "--mode auto with {TOKEN_VAR} needs --sandbox: a paired chat account's messages \
             would run edits unattended on this host"
        );
    }
    Ok(())
}

/// Where cards and notices link to: `configured` (`TELEGRAM_HOME_URL`), else the home at
/// `bind`, with this machine's loopback address in place of an unspecified one (`0.0.0.0`,
/// `::`), which no browser opens.
fn home_url(bind: SocketAddr, configured: Option<&str>) -> String {
    if let Some(home) = configured {
        return home.to_string();
    }
    let ip = match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    format!("http://{}/ui/", SocketAddr::new(ip, bind.port()))
}

/// Checks that `home` is a web address a card can link to.
fn web_address(home: &str) -> Result<()> {
    let url = reqwest::Url::parse(home)
        .with_context(|| format!("{HOME_VAR} `{home}` is not a web address"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!(
            "{HOME_VAR} `{home}` must be an http or https address, such as http://192.168.1.20:8080/ui/"
        );
    }
    Ok(())
}

/// How each answer was given, for a kernel with no signed audit log (`--ephemeral`, or a
/// build without `audit`): kept in memory while `up` runs, written by the chat thread and
/// read by the home, so the home still says "You said yes in Telegram" (spec 3.3 §8.1). The
/// trail keeps nothing, as such a kernel keeps no audit log.
pub(super) fn answers_in_memory() -> (AuditTrail, Arc<dyn AnswerLog>) {
    (AuditTrail::new(Arc::new(Unrecorded)), Arc::new(adk_surface::InMemoryAnswerLog::new()))
}

/// The audit trail of a kernel with no audit log: it records nothing.
struct Unrecorded;

#[async_trait]
impl AuditRecorder for Unrecorded {
    fn digest(&self, value: &serde_json::Value) -> String {
        adk_core::json_digest(value)
    }

    async fn record(&self, _draft: AuditRecordDraft) -> adk_core::Result<()> {
        Ok(())
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

fn adapter(token: &str, api_base: Option<&str>) -> adk_surface::channels::TelegramAdapterBuilder {
    let builder = TelegramAdapter::builder(token);
    match api_base {
        Some(base) => builder.api_base(base),
        None => builder,
    }
}

/// Paired chat accounts in the `--store` backend, on `harness.db`, or in memory with
/// `--ephemeral`, as the kernel's paired devices are.
pub(super) async fn bindings(
    setup: &Setup,
    store: Option<&str>,
) -> Result<Arc<dyn ChannelBindingStore>> {
    if let Some(url) = store {
        return shared_bindings(url).await;
    }
    #[cfg(feature = "persist")]
    if let Some(db) = &setup.db {
        let store = adk_kernel::channels::SqliteChannelBindingStore::new(db.pool.clone());
        store.migrate().await?;
        return Ok(Arc::new(store));
    }
    let _ = setup;
    Ok(Arc::new(adk_kernel::channels::InMemoryChannelBindingStore::new()))
}

/// Paired chat accounts in the `--store` backend every replica shares. Only one replica may
/// set `TELEGRAM_BOT_TOKEN`: Telegram lets one process read a bot's updates.
async fn shared_bindings(url: &str) -> Result<Arc<dyn ChannelBindingStore>> {
    let label = adk_harness::redact_store_url(url);
    match url.split_once("://").map(|(scheme, _)| scheme.to_ascii_lowercase()).as_deref() {
        #[cfg(feature = "postgres")]
        Some("postgres" | "postgresql") => {
            let store = adk_kernel::channels::PostgresChannelBindingStore::connect(url).await?;
            store.migrate().await?;
            Ok(Arc::new(store))
        }
        #[cfg(feature = "redis")]
        Some("redis" | "rediss") => {
            let store = adk_kernel::channels::RedisChannelBindingStore::connect(url).await?;
            store.migrate().await?;
            Ok(Arc::new(store))
        }
        // `Setup::open` already refused a scheme this build cannot open.
        _ => anyhow::bail!("--store {label}: no chat pairing backend for it in this build"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, PoisonError};
    use std::time::Duration;

    use adk_core::audit_log::AuditActor;
    use adk_core::{
        ApprovalPolicy, ApprovalRequest, ApprovalScope, ApproverSelector, InMemoryApprovalStore,
        NewRun, RunRecord, StoreApprovalService, ToolConfirmationRequest,
    };
    use adk_kernel::channels::{ChannelBinding, InMemoryChannelBindingStore};
    use axum::Json;
    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use chrono::{DateTime, Utc};
    use futures::stream::BoxStream;
    use serde_json::json;
    use tokio::io::AsyncReadExt;

    use super::*;

    const TOKEN: &str = "424242:AAFakeBotToken_for-tests";

    /// Spec 3.3 S-I6: a paired account's messages come from outside the machine, so Go ahead
    /// without a sandbox is refused as it is beyond loopback (on main `up` started).
    #[test]
    fn go_ahead_with_a_chat_thread_needs_a_sandbox() {
        let error = refuse_unattended(HarnessMode::Auto, false).unwrap_err();
        assert!(error.to_string().contains("--sandbox"), "{error}");
        refuse_unattended(HarnessMode::Auto, true).unwrap();
        for mode in [HarnessMode::Ask, HarnessMode::Plan, HarnessMode::ReadOnly] {
            refuse_unattended(mode, false).unwrap();
        }
    }

    /// Spec 3.3 §5.3: what chat may not decide links to the home, so the link must open (on
    /// main a wildcard bind gave `http://0.0.0.0:8080/ui/`).
    #[test]
    fn cards_link_to_an_address_a_browser_opens() {
        let home = |bind: &str| home_url(bind.parse().unwrap(), None);
        assert_eq!(home("0.0.0.0:8080"), "http://127.0.0.1:8080/ui/");
        assert_eq!(home("[::]:8080"), "http://[::1]:8080/ui/");
        assert_eq!(home("127.0.0.1:8090"), "http://127.0.0.1:8090/ui/");
        assert_eq!(home("192.168.1.20:8080"), "http://192.168.1.20:8080/ui/");
        let configured =
            home_url("0.0.0.0:8080".parse().unwrap(), Some("https://home.example/ui/"));
        assert_eq!(configured, "https://home.example/ui/");
        web_address("http://192.168.1.20:8080/ui/").unwrap();
        for bad in ["javascript:alert(1)", "home.example", "ftp://home.example/ui/"] {
            assert!(web_address(bad).is_err(), "{bad}");
        }
    }

    /// Ana's Telegram account, paired, in the group `up` routes decisions to.
    async fn paired() -> Arc<dyn ChannelBindingStore> {
        let bindings = Arc::new(InMemoryChannelBindingStore::new());
        let ana = ChannelBinding::new("telegram", "4217", "4217", "ana", Utc::now())
            .with_groups(["operator"]);
        bindings.put_binding(ana).await.unwrap();
        bindings
    }

    /// The decision `id`: a held write, routed to the operator group as `up` routes it.
    fn held_write(id: &str) -> ApprovalRequest {
        let call = ToolConfirmationRequest::new("write_file", json!({}));
        let policy = ApprovalPolicy::new([ApproverSelector::group("operator")]);
        let scope = ApprovalScope::new(APP_NAME, "ana", "s1");
        ApprovalRequest::new(id, scope, &call, policy.allow_self_approval())
    }

    /// A kernel double with an approval service and nothing else.
    struct Host {
        approvals: Arc<dyn ApprovalService>,
    }

    #[async_trait]
    impl ChannelHost for Host {
        fn app_name(&self) -> &str {
            APP_NAME
        }

        fn now(&self) -> DateTime<Utc> {
            Utc::now()
        }

        fn approvals(&self) -> Option<Arc<dyn ApprovalService>> {
            Some(self.approvals.clone())
        }

        fn audit_trail(&self) -> Option<AuditTrail> {
            None
        }

        async fn start_turn(&self, _run: NewRun) -> adk_kernel::Result<String> {
            Err(adk_kernel::KernelError::Config { reason: "this host starts no turn".into() })
        }

        async fn finished_turns(&self) -> adk_kernel::Result<BoxStream<'static, RunRecord>> {
            Ok(Box::pin(futures::stream::pending()))
        }

        async fn stop_all(&self, _reason: &str, _by: AuditActor) -> adk_kernel::Result<()> {
            Ok(())
        }
    }

    /// `up` serves the home without waiting on Telegram: with a Bot API that never answers,
    /// the chat thread starts at once and the waiting decision's card is sent in the
    /// background (on main `start` waited for every card, 30 s each).
    #[tokio::test]
    async fn the_home_does_not_wait_for_telegram() {
        // A Bot API that reads each request and never answers.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let (requests, mut seen) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((mut socket, _)) = listener.accept().await {
                let requests = requests.clone();
                held.push(tokio::spawn(async move {
                    let mut head = vec![0; 4096];
                    let read = socket.read(&mut head).await.unwrap_or_default();
                    let _ = requests.send(String::from_utf8_lossy(&head[..read]).to_string());
                    std::future::pending::<()>().await;
                    drop(socket);
                }));
            }
        });
        let approvals: Arc<dyn ApprovalService> =
            Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())));
        approvals.submit(held_write("decision-1")).await.unwrap();
        let chat = Chat {
            token: TOKEN.into(),
            api_base: Some(api),
            home: None,
            doorbell: Arc::new(ChannelDoorbell::new()),
            bindings: paired().await,
            card_hints: Vec::new(),
        };
        let host: Arc<dyn ChannelHost> = Arc::new(Host { approvals: approvals.clone() });
        let bind = "127.0.0.1:8080".parse().unwrap();
        let started =
            tokio::time::timeout(Duration::from_secs(5), chat.start(host, approvals, None, bind))
                .await;
        assert!(matches!(started, Ok(Ok(()))), "the chat thread started without waiting");
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(request) = seen.recv().await {
                if request.contains("/sendMessage ") {
                    return;
                }
            }
            panic!("the Bot API stopped")
        })
        .await
        .expect("the waiting card is sent in the background");
        server.abort();
    }

    /// The methods a fake Bot API was called with, in order.
    type Calls = Arc<Mutex<Vec<String>>>;

    /// A Bot API that refuses the token on `getUpdates`, as Telegram does once the token is
    /// revoked, and answers every other call.
    async fn refused_token(
        State(calls): State<Calls>,
        Path((_bot, method)): Path<(String, String)>,
    ) -> Response {
        calls.lock().unwrap_or_else(PoisonError::into_inner).push(method.clone());
        let result = match method.as_str() {
            "getUpdates" => {
                let refused =
                    json!({ "ok": false, "error_code": 401, "description": "Unauthorized" });
                return (StatusCode::UNAUTHORIZED, Json(refused)).into_response();
            }
            "sendMessage" => {
                json!({ "message_id": 1, "chat": { "id": 4217, "type": "private" }, "date": 0 })
            }
            _ => json!(true),
        };
        Json(json!({ "ok": true, "result": result })).into_response()
    }

    /// Cards outlive the polling: once a refused token ends it, a decision asked afterwards
    /// still reaches the thread instead of piling up in the doorbell (on main the gateway
    /// ended with the polling).
    #[tokio::test]
    async fn cards_reach_the_thread_after_the_polling_ends() {
        let calls = Calls::default();
        let app = axum::Router::new()
            .route("/{bot}/{method}", axum::routing::post(refused_token))
            .with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let doorbell = Arc::new(ChannelDoorbell::new());
        let approvals: Arc<dyn ApprovalService> = Arc::new(
            StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
                .with_notifier(doorbell.clone()),
        );
        let chat = Chat {
            token: TOKEN.into(),
            api_base: Some(api),
            home: None,
            doorbell,
            bindings: paired().await,
            card_hints: Vec::new(),
        };
        let host: Arc<dyn ChannelHost> = Arc::new(Host { approvals: approvals.clone() });
        let bind = "127.0.0.1:8080".parse().unwrap();
        chat.start(host, approvals.clone(), None, bind).await.unwrap();
        let called = |method: &str| {
            calls.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|m| m == method)
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            while !called("getUpdates") {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the polling reads updates");
        // The refused token has ended the polling by now.
        tokio::time::sleep(Duration::from_millis(300)).await;
        approvals.submit(held_write("decision-2")).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !called("sendMessage") {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the card is sent after the polling ended");
        server.abort();
    }
}
