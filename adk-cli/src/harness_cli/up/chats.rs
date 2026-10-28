//! The Slack, Discord and Matrix chat threads of `adk-rust up` (spec 3.5 §10 X12), each
//! started when its environment variables are set; without them nothing of it is built,
//! started or logged.
//!
//! | Platform | Variables | Transport |
//! |---|---|---|
//! | Slack | `SLACK_APP_TOKEN`, `SLACK_BOT_TOKEN` | Socket Mode |
//! | Discord | `DISCORD_BOT_TOKEN` | The Gateway |
//! | Matrix | `MATRIX_HOMESERVER`, `MATRIX_ACCESS_TOKEN` | `/sync` long polling |
//!
//! None needs an inbound port, so the kernel keeps its loopback bind (spec 3.3 §13 decision
//! 10). Their accounts are paired with `adk-rust pair accept <code>` in the store Telegram's
//! are in, and listed in the home under "Where you can say yes from". WhatsApp is not started
//! here: its webhook needs a public HTTPS address, which `up` does not serve.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use adk_core::ApprovalService;
use adk_core::audit_log::AuditTrail;
use adk_harness::HarnessMode;
use adk_kernel::channels::{
    ChannelBindingStore, ChannelDoorbell, ChannelGateway, ChannelHost, ChannelSink,
};
use adk_surface::AnswerLog;
use anyhow::{Result, bail};

use super::super::{APP_NAME, Setup};

/// The value of `name`, trimmed, when it is set and not empty.
pub(super) fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

/// The chat threads before the kernel is built: the doorbell the approval service rings, and
/// where paired accounts are kept.
pub(super) struct Chats {
    #[cfg(feature = "slack")]
    slack: Option<adk_surface::channels::SlackAdapterBuilder>,
    #[cfg(feature = "discord")]
    discord: Option<adk_surface::channels::DiscordAdapterBuilder>,
    #[cfg(feature = "matrix")]
    matrix: Option<adk_surface::channels::MatrixAdapterBuilder>,
    home: Option<String>,
    doorbell: Arc<ChannelDoorbell>,
    bindings: Arc<dyn ChannelBindingStore>,
    card_hints: Vec<Arc<dyn adk_surface::CardHints>>,
}

impl Chats {
    /// The chat threads whose variables are set, their accounts kept in `telegram`'s store
    /// when Telegram runs too, or beside the kernel's other stores; `None` when none is set.
    ///
    /// # Errors
    ///
    /// Refuses `--mode auto` without `--sandbox`, since a paired account's messages come from
    /// outside the machine (spec 3.3 S-I6), and half-set or malformed variables.
    pub(super) async fn from_env(
        setup: &Setup,
        store: Option<&str>,
        telegram: Option<Arc<dyn ChannelBindingStore>>,
    ) -> Result<Option<Self>> {
        #[cfg(feature = "slack")]
        let slack = super::slack::from_env()?;
        #[cfg(feature = "discord")]
        let discord = super::discord::from_env()?;
        #[cfg(feature = "matrix")]
        let matrix = super::matrix::from_env()?;
        let mut any = false;
        #[cfg(feature = "slack")]
        {
            any |= slack.is_some();
        }
        #[cfg(feature = "discord")]
        {
            any |= discord.is_some();
        }
        #[cfg(feature = "matrix")]
        {
            any |= matrix.is_some();
        }
        if !any {
            return Ok(None);
        }
        if setup.resolved.mode == HarnessMode::Auto && !setup.resolved.sandbox {
            bail!(
                "--mode auto with a Slack, Discord or Matrix chat needs --sandbox: a paired chat \
                 account's messages would run edits unattended on this host"
            );
        }
        let bindings = match telegram {
            Some(bindings) => bindings,
            None => super::telegram::bindings(setup, store).await?,
        };
        Ok(Some(Self {
            #[cfg(feature = "slack")]
            slack,
            #[cfg(feature = "discord")]
            discord,
            #[cfg(feature = "matrix")]
            matrix,
            home: None,
            doorbell: Arc::new(ChannelDoorbell::new()),
            bindings,
            card_hints: Vec::new(),
        }))
    }

    /// The doorbell to register on the approval service.
    pub(super) fn doorbell(&self) -> Arc<ChannelDoorbell> {
        self.doorbell.clone()
    }

    /// Where paired accounts are kept, for the control plane's pairing.
    pub(super) fn bindings(&self) -> Arc<dyn ChannelBindingStore> {
        self.bindings.clone()
    }

    /// Heads cards with `hints`' chat headings, as the home's engine does (spec 3.3 §7.1).
    #[cfg(feature = "verb-ui")]
    pub(super) fn with_card_hints(mut self, hints: Arc<dyn adk_surface::CardHints>) -> Self {
        self.card_hints.push(hints);
        self
    }

    /// Links cards to `home`, the phone's home.
    #[cfg(feature = "mobile")]
    pub(super) fn linking_to(mut self, home: String) -> Self {
        self.home = Some(home);
        self
    }

    /// Starts one gateway over `host`, the kernel, with every chat thread as a sink, and each
    /// adapter's connection. Cards read their decisions from `approvals` and link to the
    /// home; each answer is recorded on `records`, so the home says "You said yes in Slack".
    /// Returns once the gateway runs; the cards of decisions that waited through a restart
    /// are sent in the background.
    pub(super) async fn start(
        self,
        host: Arc<dyn ChannelHost>,
        approvals: Arc<dyn ApprovalService>,
        records: Option<(AuditTrail, Arc<dyn AnswerLog>)>,
        bind: SocketAddr,
    ) -> Result<()> {
        let home = self.home.clone().unwrap_or_else(|| home_url(bind));
        let mut builder = ChannelGateway::builder(host, self.bindings.clone())
            .target(APP_NAME)
            .home_url(home.clone())
            .doorbell(self.doorbell.clone());
        let mut names: Vec<&str> = Vec::new();
        let mut starts: Vec<Box<dyn FnOnce(ChannelGateway) -> Running + Send>> = Vec::new();
        macro_rules! wire {
            ($builder:expr, $name:literal) => {{
                let mut adapter = $builder.approvals(approvals.clone());
                for hints in &self.card_hints {
                    adapter = adapter.card_hints(hints.clone());
                }
                if let Some((trail, answers)) = &records {
                    adapter = adapter.audit(trail.clone()).answer_log(answers.clone());
                }
                let adapter = adapter.build()?;
                builder = builder.sink(Arc::new(adapter.clone()) as Arc<dyn ChannelSink>);
                names.push($name);
                starts.push(Box::new(move |gateway| {
                    let connection = adapter.start(gateway);
                    Box::pin(async move {
                        connection.join().await.map_err(|error| format!("{}: {error}", $name))
                    })
                }));
            }};
        }
        #[cfg(feature = "slack")]
        if let Some(slack) = self.slack {
            wire!(slack, "Slack");
        }
        #[cfg(feature = "discord")]
        if let Some(discord) = self.discord {
            wire!(discord, "Discord");
        }
        #[cfg(feature = "matrix")]
        if let Some(matrix) = self.matrix {
            wire!(matrix, "Matrix");
        }
        let gateway = builder.build();
        gateway.start().await?;
        let running: Vec<Running> =
            starts.into_iter().map(|start| start(gateway.clone())).collect();
        tokio::spawn(async move {
            // Decisions that waited through a restart reach the threads again.
            if let Err(error) = gateway.ring_waiting().await {
                tracing::warn!(error = %error, "the waiting decisions could not be sent to the chat threads");
            }
            for result in futures::future::join_all(running).await {
                if let Err(error) = result {
                    tracing::error!(error = %error, "a chat thread stopped reading messages; restart adk-rust up with working credentials");
                }
            }
            // The gateway lives as long as `up`, so cards keep reaching the other threads.
            std::future::pending::<()>().await;
            drop(gateway);
        });
        println!(
            "  chat  {}: an account that writes to the bot is sent a code; pair it with \
             `adk-rust pair accept <code>`; cards link to {home}",
            names.join(", ")
        );
        Ok(())
    }
}

/// A started adapter's connection, as its result.
type Running = std::pin::Pin<Box<dyn Future<Output = std::result::Result<(), String>> + Send>>;

/// The home at `bind`, with this machine's loopback address in place of an unspecified one,
/// which no browser opens.
fn home_url(bind: SocketAddr) -> String {
    let ip = match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    format!("http://{}/ui/", SocketAddr::new(ip, bind.port()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec 3.5 §10 X12: a decision submitted while `up` runs reaches a paired Matrix room
    /// through the chats' own doorbell and gateway, beside Telegram's.
    #[cfg(feature = "matrix")]
    #[tokio::test]
    async fn a_decision_reaches_a_paired_matrix_room() {
        use std::sync::Mutex;
        use std::time::Duration;

        use adk_core::audit_log::AuditActor;
        use adk_core::{
            ApprovalPolicy, ApprovalRequest, ApprovalScope, ApproverSelector,
            InMemoryApprovalStore, NewRun, RunRecord, StoreApprovalService,
            ToolConfirmationRequest,
        };
        use adk_kernel::channels::{ChannelBinding, InMemoryChannelBindingStore};
        use axum::Json;
        use axum::extract::{Path, State};
        use axum::routing::{get, put};
        use chrono::{DateTime, Utc};
        use futures::stream::BoxStream;
        use serde_json::{Value, json};

        type Sent = Arc<Mutex<Vec<(String, Value)>>>;
        async fn send(
            State(sent): State<Sent>,
            Path((room, _kind, _txn)): Path<(String, String, String)>,
            Json(content): Json<Value>,
        ) -> Json<Value> {
            sent.lock().unwrap().push((room, content));
            Json(json!({ "event_id": "$card" }))
        }
        async fn sync() -> Json<Value> {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Json(json!({ "next_batch": "s1", "rooms": {} }))
        }
        let sent = Sent::default();
        let app = axum::Router::new()
            .route(
                "/_matrix/client/v3/account/whoami",
                get(|| async { Json(json!({ "user_id": "@agents:hs" })) }),
            )
            .route("/_matrix/client/v3/sync", get(sync))
            .route("/_matrix/client/v3/rooms/{room}/send/{kind}/{txn}", put(send))
            .with_state(sent.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let homeserver = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        struct Host(Arc<dyn ApprovalService>);
        #[async_trait::async_trait]
        impl ChannelHost for Host {
            fn app_name(&self) -> &str {
                APP_NAME
            }
            fn now(&self) -> DateTime<Utc> {
                Utc::now()
            }
            fn approvals(&self) -> Option<Arc<dyn ApprovalService>> {
                Some(self.0.clone())
            }
            fn audit_trail(&self) -> Option<AuditTrail> {
                None
            }
            async fn start_turn(&self, _run: NewRun) -> adk_kernel::Result<String> {
                Err(adk_kernel::KernelError::Config { reason: "no turns here".into() })
            }
            async fn finished_turns(&self) -> adk_kernel::Result<BoxStream<'static, RunRecord>> {
                Ok(Box::pin(futures::stream::pending()))
            }
            async fn stop_all(&self, _reason: &str, _by: AuditActor) -> adk_kernel::Result<()> {
                Ok(())
            }
        }

        let bindings = Arc::new(InMemoryChannelBindingStore::new());
        let ana = ChannelBinding::new("matrix", "@ana:hs", "!dm:hs", "ana", Utc::now())
            .with_groups(["operator"]);
        bindings.put_binding(ana).await.unwrap();
        let chats = Chats {
            #[cfg(feature = "slack")]
            slack: None,
            #[cfg(feature = "discord")]
            discord: None,
            matrix: Some(adk_surface::channels::MatrixAdapter::builder(homeserver, "syt_test")),
            home: None,
            doorbell: Arc::new(ChannelDoorbell::new()),
            bindings,
            card_hints: Vec::new(),
        };
        let approvals: Arc<dyn ApprovalService> = Arc::new(
            StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
                .with_notifier(chats.doorbell()),
        );
        let host: Arc<dyn ChannelHost> = Arc::new(Host(approvals.clone()));
        let started = tokio::time::timeout(
            Duration::from_secs(5),
            chats.start(host, approvals.clone(), None, "127.0.0.1:8080".parse().unwrap()),
        )
        .await;
        assert!(matches!(started, Ok(Ok(()))), "the chats started without waiting");
        let call = ToolConfirmationRequest::new("write_file", json!({ "path": "invoice.txt" }));
        let policy =
            ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval();
        approvals
            .submit(ApprovalRequest::new(
                "decision-1",
                ApprovalScope::new(APP_NAME, "ana", "s1"),
                &call,
                policy,
            ))
            .await
            .unwrap();
        let card = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let card = sent.lock().unwrap().iter().find(|(room, _)| room == "!dm:hs").cloned();
                if let Some(card) = card {
                    return card;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the card reached the room");
        let body = card.1["body"].as_str().unwrap();
        assert!(body.starts_with("Waiting for you"), "{body}");
        assert!(body.contains("Open in home: http://127.0.0.1:8080/ui"), "{body}");
        assert!(body.contains("\nWrite file\n"), "headed by the call in plain words: {body}");
        assert!(!card.1.to_string().contains("invoice.txt"), "no argument left the machine");
        server.abort();
    }

    #[test]
    fn cards_link_to_an_address_a_browser_opens() {
        assert_eq!(home_url("0.0.0.0:8080".parse().unwrap()), "http://127.0.0.1:8080/ui/");
        assert_eq!(home_url("[::]:8080".parse().unwrap()), "http://[::1]:8080/ui/");
        assert_eq!(home_url("192.168.1.20:8080".parse().unwrap()), "http://192.168.1.20:8080/ui/");
    }

    /// Spec 3.5 §10 X12: a platform starts only when all of its variables are set; half a
    /// pair is an error, and none is nothing.
    #[test]
    fn a_platform_starts_only_with_all_of_its_variables() {
        // SAFETY: this test alone sets these variables, before reading them.
        let vars = [
            "SLACK_APP_TOKEN",
            "SLACK_BOT_TOKEN",
            "DISCORD_BOT_TOKEN",
            "MATRIX_HOMESERVER",
            "MATRIX_ACCESS_TOKEN",
        ];
        for var in vars {
            unsafe { std::env::remove_var(var) };
        }
        #[cfg(feature = "slack")]
        {
            assert!(super::super::slack::from_env().unwrap().is_none());
            unsafe { std::env::set_var("SLACK_APP_TOKEN", "xapp-1-test") };
            let error = super::super::slack::from_env().unwrap_err();
            assert!(error.to_string().contains("SLACK_BOT_TOKEN"), "{error}");
            unsafe { std::env::set_var("SLACK_BOT_TOKEN", "xoxb-1-test") };
            assert!(super::super::slack::from_env().unwrap().is_some());
        }
        #[cfg(feature = "discord")]
        {
            assert!(super::super::discord::from_env().unwrap().is_none());
            unsafe { std::env::set_var("DISCORD_BOT_TOKEN", "a b") };
            assert!(super::super::discord::from_env().is_err(), "a malformed token");
            unsafe { std::env::set_var("DISCORD_BOT_TOKEN", "test-token") };
            assert!(super::super::discord::from_env().unwrap().is_some());
        }
        #[cfg(feature = "matrix")]
        {
            assert!(super::super::matrix::from_env().unwrap().is_none());
            unsafe { std::env::set_var("MATRIX_HOMESERVER", "http://matrix.example.org") };
            assert!(super::super::matrix::from_env().is_err(), "half the pair");
            unsafe { std::env::set_var("MATRIX_ACCESS_TOKEN", "syt_test") };
            let error = super::super::matrix::from_env().unwrap_err();
            assert!(
                error.to_string().contains("MATRIX_HOMESERVER"),
                "plain http beyond loopback: {error}"
            );
            assert!(!format!("{error:#}").contains("syt_test"), "{error:#}");
            unsafe { std::env::set_var("MATRIX_HOMESERVER", "https://matrix.example.org") };
            assert!(super::super::matrix::from_env().unwrap().is_some());
        }
        for var in vars {
            unsafe { std::env::remove_var(var) };
        }
    }
}
