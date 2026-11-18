//! The Microsoft Teams adapter over a fake Bot Connector, Entra ID and OpenID metadata, under a
//! gateway over the contract suite's kernel double: every activity's token is checked before
//! it is read — signature, issuer, audience, validity with clock skew, endorsement and service
//! address — and the keys are cached; a stranger gets a code; a pressed `Action.Execute` (or
//! its `Action.Submit` fallback) decides once and a second press is shown; typed text never
//! answers; group chats are not read; a redelivery is taken once; a single-tenant bot takes
//! its own tenant only; a refused access token is renewed once; throttling is waited out only
//! for a paired person; and a wake-up reaches a conversation the adapter has not heard from.
//!
//! The `#[ignore]`d live test runs a kernel-hosted harness agent on `claude-haiku-5-5` behind
//! the adapter: a simulated Teams person pairs, asks a question and gets the model's answer,
//! then asks for a write, which reaches Teams as an Adaptive Card; Allow once runs it once.

#![cfg(feature = "teams")]

mod chat_support;
mod fake_teams;

use std::sync::Arc;
use std::time::Duration;

use adk_core::{AdkError, ApprovalRisk, ApprovalState};
use adk_kernel::channels::{ChannelGateway, ChannelSink, InMemoryChannelBindingStore};
use adk_kernel::conformance::ContractHost;
use adk_surface::channels::{TeamsAdapter, TeamsWebhook};
use axum::http::StatusCode;
use chat_support::{eventually, pair, world};
use fake_teams::{FakeTeams, TENANT, connector_key, forged_key, post_to, shown};
use serde_json::{Value, json};

const ANA: &str = "29:1XJKJMvc5GBtc2JwZq0oj8tHZmzrQgFmB39ATiQWA85gQtHieVkKilBZ9XHoq9j7Zaqt7CZ";
const ANA_CHAT: &str = "a:17I0kl9EkpE1O9PH5TWrzrLNwnWWcfrU7QZjKR0WSfOpzbfcAg2Iayd";
const STRANGER: &str = "29:stranger";
const STRANGER_CHAT: &str = "a:stranger-dm";

struct World {
    api: FakeTeams,
    adapter: TeamsAdapter,
    host: Arc<ContractHost>,
    bindings: Arc<InMemoryChannelBindingStore>,
    gateway: ChannelGateway,
    webhook: TeamsWebhook,
}

async fn started_with(api: FakeTeams, adapter: TeamsAdapter) -> World {
    let (host, bindings, gateway) = world(Arc::new(adapter.clone())).await;
    let webhook = adapter.start(gateway.clone());
    World { api, adapter, host, bindings, gateway, webhook }
}

async fn started() -> World {
    let api = FakeTeams::start().await;
    let adapter = api.builder().build().unwrap();
    started_with(api, adapter).await
}

impl World {
    async fn paired() -> Self {
        let world = started().await;
        pair(&world.bindings, "teams", ANA, ANA_CHAT, "ana").await;
        world
    }

    /// Delivers `activity` with a valid Bot Connector token.
    async fn deliver(&self, activity: &Value) -> (StatusCode, Value) {
        post_to(&self.webhook.router(), activity, Some(&self.api.token())).await
    }

    async fn say(&self, from: &str, conversation: &str, text: &str) -> StatusCode {
        self.deliver(&self.api.message(from, conversation, text)).await.0
    }

    async fn shown_in(&self, conversation: &str, text: &str) -> String {
        eventually(&format!("\"{text}\" in {conversation}"), || async {
            self.api.texts(conversation).into_iter().find(|shown| shown.contains(text))
        })
        .await
    }

    async fn card(&self, risk: ApprovalRisk) -> adk_core::ApprovalRequest {
        let request =
            self.host.ask("Send the invoice", json!({ "to": "client@example.com" }), risk).await;
        assert_eq!(self.gateway.ring(&request).await.unwrap(), 1);
        eventually("the card", || async { self.api.last_card(ANA_CHAT) }).await;
        request
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_check_on_the_connector_token_holds_before_the_activity_is_read() {
    let w = started().await;
    let router = w.webhook.router();
    let activity = w.api.message(STRANGER, STRANGER_CHAT, "hello");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let cases: Vec<(&str, Option<String>, StatusCode)> = vec![
        ("no token", None, StatusCode::UNAUTHORIZED),
        ("not a JWT", Some("not-a-jwt".into()), StatusCode::UNAUTHORIZED),
        (
            "signed by a key nobody published",
            Some(w.api.token_with(json!({}), forged_key())),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "another bot's audience",
            Some(w.api.token_with(json!({ "aud": "another-app" }), connector_key())),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "another issuer",
            Some(w.api.token_with(json!({ "iss": "https://sts.windows.net/x/" }), connector_key())),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "expired beyond the skew",
            Some(w.api.token_with(json!({ "exp": now - 600, "nbf": now - 4000 }), connector_key())),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "not yet valid",
            Some(w.api.token_with(json!({ "nbf": now + 900 }), connector_key())),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "no expiry",
            Some(w.api.token_with(json!({ "exp": null }), connector_key())),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "another service address",
            Some(w.api.token_with(
                json!({ "serviceurl": "https://attacker.example/teams/" }),
                connector_key(),
            )),
            StatusCode::FORBIDDEN,
        ),
        (
            "no service address",
            Some(w.api.token_with(json!({ "serviceurl": null }), connector_key())),
            StatusCode::FORBIDDEN,
        ),
    ];
    for (case, token, status) in cases {
        assert_eq!(post_to(&router, &activity, token.as_deref()).await.0, status, "{case}");
    }
    // A channel the key does not endorse.
    let webchat = w.api.activity(
        "message",
        STRANGER,
        STRANGER_CHAT,
        json!({ "channelId": "webchat", "text": "hi" }),
    );
    assert_eq!(w.deliver(&webchat).await.0, StatusCode::FORBIDDEN, "not endorsed");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(w.api.sent().is_empty(), "nothing reached the gateway: {:?}", w.api.sent());
    // Within the five minutes of skew a token is still taken, and the stranger gets a code.
    let skewed = w.api.token_with(json!({ "exp": now - 60 }), connector_key());
    assert_eq!(post_to(&router, &activity, Some(&skewed)).await.0, StatusCode::OK);
    let code = w.shown_in(STRANGER_CHAT, "adk-rust pair accept").await;
    assert!(chat_support::code_in(&code).is_some(), "{code}");
    assert!(w.host.turns().is_empty(), "a stranger starts nothing");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_signing_keys_are_cached_and_forged_key_ids_cannot_make_the_adapter_hammer_them() {
    let w = World::paired().await;
    for text in ["one", "two", "three"] {
        assert_eq!(w.say(ANA, ANA_CHAT, text).await, StatusCode::OK);
    }
    assert_eq!(w.api.key_fetches().await, 1, "fetched once for three activities");
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    let activity = w.api.message(ANA, ANA_CHAT, "four");
    for index in 0..5 {
        header.kid = Some(format!("forged-{index}"));
        let claims = json!({ "iss": fake_teams::ISSUER, "aud": fake_teams::APP_ID, "exp": 4102444800u64, "serviceurl": w.api.service_url() });
        let unknown = jsonwebtoken::encode(&header, &claims, &forged_key().encoding).unwrap();
        assert_eq!(
            post_to(&w.webhook.router(), &activity, Some(&unknown)).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        w.api.key_fetches().await,
        1,
        "unknown key ids fetch the keys at most once every five minutes"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pressed_card_action_decides_once_and_replies_to_the_card() {
    let w = World::paired().await;
    let request = w.card(ApprovalRisk::Medium).await;
    let card = w.api.last_card(ANA_CHAT).unwrap();
    let card_text = card.body.to_string();
    assert!(!card_text.contains("client@example.com"), "no argument left the machine: {card_text}");
    assert!(shown(&card.body).starts_with("Waiting for you · since "), "{}", shown(&card.body));
    let press = w.api.press(ANA, ANA_CHAT, "Allow once").expect("an Allow once button");
    let (status, body) = w.deliver(&press).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "statusCode": 200, "type": "application/vnd.microsoft.activity.message", "value": "Answer received." })
    );
    eventually("the decision", || async {
        (w.host.decision(&request.id).await.state == ApprovalState::Approved).then_some(())
    })
    .await;
    let confirmed = w.shown_in(ANA_CHAT, "You allowed it once").await;
    assert!(!confirmed.contains("Send the invoice"), "a reply to the card need not repeat it");
    let reply = w
        .api
        .sent_to(ANA_CHAT)
        .into_iter()
        .rev()
        .find(|sent| shown(&sent.body).contains("You allowed it once"))
        .unwrap();
    assert_eq!(reply.reply_to, card.id, "the confirmation replies to the card");
    assert_eq!(reply.body["textFormat"], "plain");
    // A second press is shown, never retried (S-I2).
    let again = w.api.press(ANA, ANA_CHAT, "Allow once").unwrap();
    assert_eq!(w.deliver(&again).await.0, StatusCode::OK);
    w.shown_in(ANA_CHAT, "Decided already").await;
    assert_eq!(w.host.decision(&request.id).await.decisions.len(), 1, "decided once");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_older_clients_action_submit_answers_too() {
    let w = World::paired().await;
    let request = w.card(ApprovalRisk::Low).await;
    let submit = w.api.submit(ANA, ANA_CHAT, "Deny").expect("the Action.Submit fallback");
    assert_eq!(w.deliver(&submit).await, (StatusCode::OK, Value::Null));
    eventually("the decision", || async {
        (w.host.decision(&request.id).await.state == ApprovalState::Rejected).then_some(())
    })
    .await;
    w.shown_in(ANA_CHAT, "You denied it").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typed_text_never_answers_and_a_foreign_card_action_is_refused() {
    let w = World::paired().await;
    let request = w.card(ApprovalRisk::Low).await;
    let data = w.api.last_card(ANA_CHAT).unwrap().body["attachments"][0]["content"]["body"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["actions"][0]["data"]["adk"]
        .clone();
    assert_eq!(w.say(ANA, ANA_CHAT, data.as_str().unwrap()).await, StatusCode::OK);
    eventually("a turn from the typed text", || async {
        (!w.host.turns().is_empty()).then_some(())
    })
    .await;
    assert_eq!(
        w.host.decision(&request.id).await.state,
        ApprovalState::Pending,
        "typed text is only text"
    );
    let foreign = w.api.activity(
        "invoke",
        ANA,
        ANA_CHAT,
        json!({ "name": "adaptiveCard/action", "value": { "action": { "type": "Action.Execute", "verb": "someone.else", "data": { "adk": data } } } }),
    );
    let (status, body) = w.deliver(&foreign).await;
    assert_eq!((status, &body["statusCode"]), (StatusCode::OK, &json!(400)), "{body}");
    assert_eq!(w.host.decision(&request.id).await.state, ApprovalState::Pending);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_redelivered_activity_is_taken_once_and_group_chats_are_not_read() {
    let w = World::paired().await;
    let activity = w.api.message(ANA, ANA_CHAT, "draft the reply");
    assert_eq!(w.deliver(&activity).await.0, StatusCode::OK);
    assert_eq!(w.deliver(&activity).await.0, StatusCode::OK, "acknowledged again");
    eventually("the turn", || async { (w.host.turns().len() == 1).then_some(()) }).await;
    let group = w.api.activity(
        "message",
        ANA,
        "19:group@thread.v2",
        json!({ "text": "stop all", "conversation": { "conversationType": "groupChat", "isGroup": true, "id": "19:group@thread.v2", "tenantId": TENANT } }),
    );
    assert_eq!(w.deliver(&group).await.0, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(w.host.turns().len(), 1, "one turn");
    assert!(!w.host.is_stopped(), "a group chat's text reached nothing");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_single_tenant_bot_takes_its_own_tenant_and_its_token_from_there() {
    let api = FakeTeams::start().await;
    let adapter = api.builder().tenant(TENANT).build().unwrap();
    let w = started_with(api, adapter).await;
    pair(&w.bindings, "teams", ANA, ANA_CHAT, "ana").await;
    let other = w.api.activity(
        "message",
        ANA,
        ANA_CHAT,
        json!({ "text": "hi", "conversation": { "conversationType": "personal", "tenantId": "another-tenant", "id": ANA_CHAT }, "channelData": { "tenant": { "id": "another-tenant" } } }),
    );
    assert_eq!(w.deliver(&other).await.0, StatusCode::FORBIDDEN);
    w.adapter.send_message(ANA_CHAT, "Good morning.").await.unwrap();
    let requests = w.api.server.received_requests().await.unwrap();
    assert!(
        requests.iter().any(|request| request.url.path() == format!("/{TENANT}/oauth2/v2.0/token")),
        "the token came from the bot's tenant"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_access_token_is_cached_and_renewed_once_when_refused() {
    let w = World::paired().await;
    w.adapter.send_message(ANA_CHAT, "one").await.unwrap();
    w.adapter.send_message(ANA_CHAT, "two").await.unwrap();
    assert_eq!(w.api.token_requests().await, 1, "cached");
    w.api.refuse(1, 401, None);
    w.adapter.send_message(ANA_CHAT, "three").await.unwrap();
    assert_eq!(w.api.token_requests().await, 2, "renewed once");
    assert_eq!(w.api.texts(ANA_CHAT).last().map(String::as_str), Some("three"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refused_credentials_say_what_to_set_and_never_echo_the_password() {
    let api = FakeTeams::start().await;
    let adapter = TeamsAdapter::builder(fake_teams::APP_ID, "the-wrong-secret")
        .login_base(api.server.uri())
        .openid_configuration(api.openid_configuration())
        .service_url(api.service_url())
        .build()
        .unwrap();
    let error = AdkError::from(adapter.send_message(ANA_CHAT, "hi").await.unwrap_err());
    assert_eq!(error.code, "surface.teams.unauthorized");
    assert!(error.message.contains("TEAMS_APP_PASSWORD"), "{}", error.message);
    assert!(!error.to_string().contains("the-wrong-secret"));
    assert!(api.sent().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn throttling_is_waited_out_for_a_paired_person_only() {
    let w = World::paired().await;
    w.api.refuse(1, 429, Some(1));
    let started = tokio::time::Instant::now();
    w.adapter.send_message(ANA_CHAT, "after the wait").await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(900), "Retry-After was honoured");
    assert_eq!(w.api.texts(ANA_CHAT).last().map(String::as_str), Some("after the wait"));
    // A stranger's pairing code is not retried while throttled.
    w.api.refuse(1, 429, Some(1));
    assert_eq!(w.say(STRANGER, STRANGER_CHAT, "hello").await, StatusCode::OK);
    eventually("the throttled code", || async {
        (!w.api.sent_to(STRANGER_CHAT).is_empty()).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let sent = w.api.sent_to(STRANGER_CHAT);
    assert_eq!(sent.len(), 1, "tried once: {sent:?}");
    assert!(sent[0].id.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wake_up_reaches_a_conversation_heard_from_before_a_restart() {
    // A new adapter has heard from no one: the card goes to the configured service address.
    let w = World::paired().await;
    let request = w.card(ApprovalRisk::Low).await;
    let card = w.api.last_card(ANA_CHAT).unwrap();
    assert!(shown(&card.body).contains("Finance"), "{}", shown(&card.body));
    assert!(card.body.to_string().contains(&request.id));
    assert!(card.reply_to.is_none(), "a new message, not a reply");
}

// ---------------------------------------------------------------------------
// Live: a kernel-hosted harness agent on claude-haiku-5-5 behind the adapter
// ---------------------------------------------------------------------------

mod live {
    use std::path::PathBuf;

    use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
    use adk_core::{
        ApprovalPolicy, ApprovalService, ApproverSelector, DurableRunStore, InMemoryApprovalStore,
        InMemoryRunStore, Llm, StoreApprovalService,
    };
    use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig};
    use adk_kernel::channels::{ChannelDoorbell, accept_pairing};
    use adk_kernel::control::ControlPlaneConfig;
    use adk_kernel::{InMemoryProcessStore, Kernel, KernelConfig, ProcessStore};
    use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
    use adk_runner::Runner;
    use adk_server::background::DurableWorkerConfig;
    use adk_session::{InMemorySessionService, SessionService};

    use super::*;

    const APP: &str = "home";
    const WITHIN: Duration = Duration::from_secs(180);

    async fn within<T, F, Fut>(what: &str, mut probe: F) -> T
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Option<T>>,
    {
        let started = tokio::time::Instant::now();
        while started.elapsed() <= WITHIN {
            if let Some(value) = probe().await {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("timed out waiting for {what}");
    }

    struct Live {
        _dir: tempfile::TempDir,
        path: PathBuf,
        _kernel: Kernel,
        service: Arc<dyn ApprovalService>,
        bindings: Arc<InMemoryChannelBindingStore>,
        api: FakeTeams,
        webhook: TeamsWebhook,
    }

    async fn live(model: Arc<dyn Llm>) -> Live {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
        let doorbell = Arc::new(ChannelDoorbell::new());
        let service: Arc<dyn ApprovalService> = Arc::new(
            StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
                .with_run_store(Arc::clone(&runs))
                .with_router(
                    ApprovalPolicy::new([ApproverSelector::group("operator")])
                        .allow_self_approval(),
                )
                .with_notifier(doorbell.clone()),
        );
        let agent = Arc::new(
            HarnessAgent::builder("finance")
                .model(model)
                .workspace(adk_devtools::Workspace::new(&path))
                .project_instructions(InstructionsConfig {
                    user_file: None,
                    require_trust: false,
                    ..InstructionsConfig::default()
                })
                .without_compaction()
                .without_default_members()
                .mode(HarnessMode::Ask)
                .approval_service(Arc::clone(&service))
                .build()
                .unwrap(),
        );
        let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let audit = InMemoryAuditRecorder::new();
        let runner = agent
            .configure_runner(
                Runner::builder()
                    .app_name(APP)
                    .agent(agent.clone())
                    .session_service(Arc::clone(&sessions)),
            )
            .audit_log(Arc::new(audit.clone()))
            .build()
            .unwrap();
        let harness = Arc::new(
            Harness::with_runner(agent, runner).unwrap().with_run_store(Arc::clone(&runs)),
        );
        let kernel = harness
            .kernel()
            .config(
                KernelConfig::default()
                    .with_app_name(APP)
                    .with_worker(
                        DurableWorkerConfig::default()
                            .with_worker_id("k1")
                            .with_poll_interval(Duration::from_millis(50)),
                    )
                    .with_scheduler_tick(Duration::from_millis(100))
                    .with_control(ControlPlaneConfig::default().without_socket()),
            )
            .process_store(Arc::new(InMemoryProcessStore::new()) as Arc<dyn ProcessStore>)
            .audit(AuditTrail::new(Arc::new(audit)))
            .build()
            .unwrap();
        kernel.start().await.unwrap();
        let api = FakeTeams::start().await;
        let teams = api.builder().approvals(Arc::clone(&service)).build().unwrap();
        let bindings = Arc::new(InMemoryChannelBindingStore::new());
        let gateway = ChannelGateway::builder(Arc::new(kernel.clone()), bindings.clone())
            .sink(Arc::new(teams.clone()))
            .target(APP)
            .home_url("http://127.0.0.1:8080/ui/")
            .doorbell(doorbell)
            .build();
        gateway.start().await.unwrap();
        let webhook = teams.start(gateway);
        Live { _dir: dir, path, _kernel: kernel, service, bindings, api, webhook }
    }

    impl Live {
        async fn deliver(&self, activity: &Value) -> (StatusCode, Value) {
            post_to(&self.webhook.router(), activity, Some(&self.api.token())).await
        }

        async fn shown_after(
            &self,
            before: usize,
            what: &str,
            test: impl Fn(&str) -> bool,
        ) -> String {
            within(what, || async {
                self.api.texts(ANA_CHAT).into_iter().skip(before).find(|text| test(text))
            })
            .await
        }
    }

    fn transcript(api: &FakeTeams) {
        for sent in api.sent() {
            let kind = if sent.body["attachments"].is_array() { "card" } else { "text" };
            let reply =
                sent.reply_to.as_deref().map(|id| format!(" (reply to {id})")).unwrap_or_default();
            println!(
                "[teams → {} {kind}{reply}] {}",
                sent.conversation,
                shown(&sent.body).replace('\n', " | ")
            );
        }
    }

    /// A simulated Teams person pairs, gets claude-haiku-5-5's answer, and approves a held
    /// write from its Adaptive Card; the write runs once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "calls the Anthropic API"]
    async fn a_live_agent_answers_a_teams_activity_and_a_card_round_trips() {
        let Some(vars) = chat_support::live_env(&["ANTHROPIC_API_KEY"]) else {
            panic!("ANTHROPIC_API_KEY is not set");
        };
        let model: Arc<dyn Llm> = Arc::new(
            AnthropicClient::new(AnthropicConfig::new(vars[0].clone(), "claude-haiku-5-5"))
                .unwrap(),
        );
        let l = live(model).await;
        // A stranger gets a code, which the operator accepts.
        let (status, _) = l.deliver(&l.api.message(ANA, ANA_CHAT, "hello")).await;
        assert_eq!(status, StatusCode::OK);
        let code = l
            .shown_after(0, "the pairing code", |text| text.contains("adk-rust pair accept"))
            .await;
        let code = chat_support::code_in(&code).unwrap();
        accept_pairing(
            l.bindings.as_ref(),
            &code,
            "ana",
            &["operator".to_string()],
            chrono::Utc::now(),
        )
        .await
        .unwrap();

        // A question: the model's answer comes back to the conversation as plain text.
        let before = l.api.sent().len();
        let started = std::time::Instant::now();
        l.deliver(&l.api.message(
            ANA,
            ANA_CHAT,
            "What is 17 times 23? Reply with just the number.",
        ))
        .await;
        let answer = l.shown_after(before, "the model's answer", |text| text.contains("391")).await;
        println!("answered in {:?}: {answer}", started.elapsed());

        // A write: held, carded in Teams, allowed once from the card.
        let before = l.api.sent().len();
        l.deliver(&l.api.message(
            ANA,
            ANA_CHAT,
            "Create the file invoice.txt containing exactly INV-1, using write_file. Then reply with one short sentence.",
        ))
        .await;
        let card = within("the card", || async {
            l.api.sent().into_iter().skip(before).find(|sent| sent.body["attachments"].is_array())
        })
        .await;
        println!("card: {}", serde_json::to_string_pretty(&card.body).unwrap());
        assert!(shown(&card.body).contains("Write file"), "{}", shown(&card.body));
        assert!(!card.body.to_string().contains("INV-1"), "no argument left the machine");
        assert!(!l.path.join("invoice.txt").exists(), "held until answered");
        let press = l.api.press(ANA, ANA_CHAT, "Allow once").expect("Allow once");
        let (status, body) = l.deliver(&press).await;
        assert_eq!((status, body["value"].as_str()), (StatusCode::OK, Some("Answer received.")));
        l.shown_after(before, "the confirmation", |text| text.contains("You allowed it once"))
            .await;
        let written = within("the write", || async {
            std::fs::read_to_string(l.path.join("invoice.txt")).ok()
        })
        .await;
        assert_eq!(written.trim(), "INV-1");
        let last_card = l.api.last_card(ANA_CHAT).unwrap();
        let after = l
            .shown_after(before, "the agent's reply after the write", |text| {
                !text.starts_with("Waiting for you") && !text.contains("You allowed it once")
            })
            .await;
        println!("reply: {after}");
        let pending = l.service.inbox(adk_core::InboxQuery::default()).await.unwrap().items;
        assert!(pending.is_empty(), "nothing still waits: {pending:?}");
        assert_eq!(last_card.id, card.id, "one card");
        transcript(&l.api);
    }
}
