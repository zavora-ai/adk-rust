//! The Slack, Discord, Matrix and Microsoft Teams chat threads of `adk-rust up` (spec 3.5 §10
//! X12), each started when its environment variables are set; without them nothing of it is
//! built, started or logged.
//!
//! | Platform | Variables | Transport |
//! |---|---|---|
//! | Slack | `SLACK_APP_TOKEN`, `SLACK_BOT_TOKEN` | Socket Mode |
//! | Discord | `DISCORD_BOT_TOKEN` | The Gateway |
//! | Matrix | `MATRIX_HOMESERVER`, `MATRIX_ACCESS_TOKEN` | `/sync` long polling |
//! | Microsoft Teams | `TEAMS_APP_ID`, `TEAMS_APP_PASSWORD` (`TEAMS_TENANT_ID`, `TEAMS_BIND`, `TEAMS_SERVICE_URL`) | The Bot Framework messaging endpoint on a listener of its own |
//!
//! Slack, Discord and Matrix need no inbound port, so the kernel keeps its loopback bind (spec
//! 3.3 §13 decision 10). Teams' endpoint listens on `TEAMS_BIND` and serves only
//! `POST /api/messages`, each request's Bot Connector token checked, so publishing it never
//! publishes the home. Accounts are paired with `adk-rust pair accept <code>` in the store
//! Telegram's are in, and listed in the home under "Where you can say yes from". WhatsApp is
//! not started here: its webhook belongs to a served site's public edge.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use adk_core::ApprovalService;
use adk_core::audit_log::AuditTrail;
use adk_harness::HarnessMode;
use adk_kernel::channels::{
    ChannelBindingStore, ChannelDoorbell, ChannelGateway, ChannelHost, ChannelSink,
};
use adk_surface::AnswerLog;
use anyhow::{Context, Result, bail};

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
    #[cfg(feature = "teams")]
    teams: Option<super::teams::Teams>,
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
        #[cfg(feature = "teams")]
        let teams = super::teams::from_env()?;
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
        #[cfg(feature = "teams")]
        {
            any |= teams.is_some();
        }
        if !any {
            return Ok(None);
        }
        if setup.resolved.mode == HarnessMode::Auto && !setup.resolved.sandbox {
            bail!(
                "--mode auto with a Slack, Discord, Matrix or Microsoft Teams chat needs \
                 --sandbox: a paired chat account's messages would run edits unattended on this \
                 host"
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
            #[cfg(feature = "teams")]
            teams,
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
        // Teams' messaging endpoint, on its own listener; bound before anything starts.
        #[cfg(feature = "teams")]
        let mut endpoint = None;
        #[cfg(feature = "teams")]
        if let Some(teams) = self.teams {
            let mut adapter = teams.builder.approvals(approvals.clone());
            for hints in &self.card_hints {
                adapter = adapter.card_hints(hints.clone());
            }
            if let Some((trail, answers)) = &records {
                adapter = adapter.audit(trail.clone()).answer_log(answers.clone());
            }
            let adapter = adapter.build()?;
            let listener = tokio::net::TcpListener::bind(teams.bind).await.with_context(|| {
                format!(
                    "Microsoft Teams' messaging endpoint cannot listen on {}: set TEAMS_BIND",
                    teams.bind
                )
            })?;
            endpoint = Some(listener.local_addr()?);
            builder = builder.sink(Arc::new(adapter.clone()) as Arc<dyn ChannelSink>);
            names.push("Microsoft Teams");
            starts.push(Box::new(move |gateway| {
                let webhook = adapter.start(gateway);
                let app = axum::Router::new().nest(super::teams::PATH, webhook.router());
                Box::pin(async move {
                    let served = axum::serve(listener, app).await;
                    drop(webhook);
                    served.map_err(|error| format!("Microsoft Teams: {error}"))
                })
            }));
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
        #[cfg(feature = "teams")]
        if let Some(endpoint) = endpoint {
            println!(
                "  teams http://{endpoint}{}: publish it over HTTPS as the bot's messaging \
                 endpoint (a tunnel or reverse proxy to this port alone)",
                super::teams::PATH
            );
        }
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

    /// The kernel as the chats see it: its approval service, and no turns.
    #[cfg(any(feature = "matrix", feature = "teams"))]
    struct Host(Arc<dyn ApprovalService>);

    #[cfg(any(feature = "matrix", feature = "teams"))]
    #[async_trait::async_trait]
    impl ChannelHost for Host {
        fn app_name(&self) -> &str {
            APP_NAME
        }
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            chrono::Utc::now()
        }
        fn approvals(&self) -> Option<Arc<dyn ApprovalService>> {
            Some(self.0.clone())
        }
        fn audit_trail(&self) -> Option<AuditTrail> {
            None
        }
        async fn start_turn(&self, _run: adk_core::NewRun) -> adk_kernel::Result<String> {
            Err(adk_kernel::KernelError::Config { reason: "no turns here".into() })
        }
        async fn finished_turns(
            &self,
        ) -> adk_kernel::Result<futures::stream::BoxStream<'static, adk_core::RunRecord>> {
            Ok(Box::pin(futures::stream::pending()))
        }
        async fn stop_all(
            &self,
            _reason: &str,
            _by: adk_core::audit_log::AuditActor,
        ) -> adk_kernel::Result<()> {
            Ok(())
        }
    }

    /// A decision submitted while `up` runs reaches a paired Teams chat as an Adaptive Card
    /// through the chats' own doorbell, posted with an Entra ID token; the messaging endpoint
    /// listens on its own address, serves `POST /api/messages` alone, and refuses a request
    /// without a Bot Connector token before reading it.
    #[cfg(feature = "teams")]
    #[tokio::test]
    async fn a_decision_reaches_a_paired_teams_chat_and_the_endpoint_checks_every_request() {
        use std::sync::Mutex;
        use std::time::Duration;

        use adk_core::{
            ApprovalPolicy, ApprovalRequest, ApprovalScope, ApproverSelector,
            InMemoryApprovalStore, StoreApprovalService, ToolConfirmationRequest,
        };
        use adk_kernel::channels::{ChannelBinding, InMemoryChannelBindingStore};
        use axum::Json;
        use axum::extract::{Path, State};
        use axum::http::HeaderMap;
        use axum::routing::post;
        use serde_json::{Value, json};

        type Posted = Arc<Mutex<Vec<(String, Option<String>, Value)>>>;
        async fn activities(
            State(posted): State<Posted>,
            Path(conversation): Path<String>,
            headers: HeaderMap,
            Json(activity): Json<Value>,
        ) -> Json<Value> {
            let auth = headers.get("authorization").and_then(|value| value.to_str().ok());
            posted.lock().unwrap().push((conversation, auth.map(str::to_string), activity));
            Json(json!({ "id": "1763000000001" }))
        }
        let posted = Posted::default();
        let app = axum::Router::new()
            .route(
                "/botframework.com/oauth2/v2.0/token",
                post(|| async {
                    Json(json!({ "token_type": "Bearer", "expires_in": 3600, "access_token": "entra-token" }))
                }),
            )
            .route("/teams/v3/conversations/{conversation}/activities", post(activities))
            .with_state(posted.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fake = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        // A free port for the endpoint.
        let bind = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();

        let bindings = Arc::new(InMemoryChannelBindingStore::new());
        let ana = ChannelBinding::new("teams", "29:ana", "a:dm-ana", "ana", chrono::Utc::now())
            .with_groups(["operator"]);
        bindings.put_binding(ana).await.unwrap();
        let teams = super::super::teams::Teams {
            builder: adk_surface::channels::TeamsAdapter::builder(
                "11111111-2222-3333-4444-555555555555",
                "fake-secret",
            )
            .login_base(fake.clone())
            .openid_configuration(format!("{fake}/openid"))
            .service_url(format!("{fake}/teams/")),
            bind,
        };
        let chats = Chats {
            #[cfg(feature = "slack")]
            slack: None,
            #[cfg(feature = "discord")]
            discord: None,
            #[cfg(feature = "matrix")]
            matrix: None,
            teams: Some(teams),
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
        chats
            .start(host, approvals.clone(), None, "127.0.0.1:8080".parse().unwrap())
            .await
            .unwrap();
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
        let (conversation, auth, card) = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(card) = posted.lock().unwrap().first().cloned() {
                    return card;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the card reached the chat");
        assert_eq!(conversation, "a:dm-ana");
        assert_eq!(auth.as_deref(), Some("Bearer entra-token"));
        let content = &card["attachments"][0]["content"];
        assert_eq!(content["type"], "AdaptiveCard");
        assert!(content.to_string().contains("Write file"), "{content}");
        assert!(!card.to_string().contains("invoice.txt"), "no argument left the machine");

        let client = reqwest::Client::new();
        let endpoint = format!("http://{bind}/api/messages");
        let unsigned =
            client.post(&endpoint).json(&json!({ "type": "message" })).send().await.unwrap();
        assert_eq!(unsigned.status(), reqwest::StatusCode::UNAUTHORIZED);
        let home = client.get(format!("http://{bind}/ui/")).send().await.unwrap();
        assert_eq!(
            home.status(),
            reqwest::StatusCode::NOT_FOUND,
            "the endpoint serves nothing else"
        );
        server.abort();
    }

    /// Spec 3.5 §10 X12: a decision submitted while `up` runs reaches a paired Matrix room
    /// through the chats' own doorbell and gateway, beside Telegram's.
    #[cfg(feature = "matrix")]
    #[tokio::test]
    async fn a_decision_reaches_a_paired_matrix_room() {
        use std::sync::Mutex;
        use std::time::Duration;

        use adk_core::{
            ApprovalPolicy, ApprovalRequest, ApprovalScope, ApproverSelector,
            InMemoryApprovalStore, StoreApprovalService, ToolConfirmationRequest,
        };
        use adk_kernel::channels::{ChannelBinding, InMemoryChannelBindingStore};
        use axum::Json;
        use axum::extract::{Path, State};
        use axum::routing::{get, put};
        use chrono::Utc;
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
            #[cfg(feature = "teams")]
            teams: None,
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
            "TEAMS_APP_ID",
            "TEAMS_APP_PASSWORD",
            "TEAMS_TENANT_ID",
            "TEAMS_BIND",
            "TEAMS_SERVICE_URL",
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
        #[cfg(feature = "teams")]
        {
            use super::super::teams;
            assert!(teams::from_env().unwrap().is_none());
            unsafe { std::env::set_var("TEAMS_APP_ID", "11111111-2222-3333-4444-555555555555") };
            let error = teams::from_env().err().unwrap();
            assert!(error.to_string().contains("TEAMS_APP_PASSWORD"), "{error}");
            unsafe { std::env::set_var("TEAMS_APP_PASSWORD", "s3cret~value") };
            let started = teams::from_env().unwrap().unwrap();
            assert_eq!(started.bind, teams::DEFAULT_BIND.parse().unwrap());
            unsafe { std::env::set_var("TEAMS_SERVICE_URL", "http://smba.example/teams") };
            let error = teams::from_env().err().unwrap();
            assert!(!format!("{error:#}").contains("s3cret"), "{error:#}");
            unsafe { std::env::remove_var("TEAMS_SERVICE_URL") };
            unsafe { std::env::set_var("TEAMS_BIND", "port 80") };
            assert!(teams::from_env().is_err(), "a bind that is not host:port");
            unsafe { std::env::set_var("TEAMS_BIND", "0.0.0.0:3978") };
            assert_eq!(teams::from_env().unwrap().unwrap().bind.port(), 3978);
        }
        for var in vars {
            unsafe { std::env::remove_var(var) };
        }
    }
}
