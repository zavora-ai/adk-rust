//! `POST /triggers/pubsub/{name}` and `POST /triggers/eventarc/{name}`: deliveries carrying a
//! Google-signed OIDC token start one durable run each, however often they are redelivered.
//!
//! The tokens are minted with a local RSA key, and its public key is served as a JWK set from a
//! local HTTP server standing in for `https://www.googleapis.com/oauth2/v3/certs`, so every
//! check the verifier makes against Google's keys runs here: signature, issuer, audience,
//! lifetime, and the service-account allowlist.

#![cfg(all(feature = "google-triggers", feature = "background"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_auth::federation::{AgentTokenSigner, RsaTokenSigner};
use adk_auth::sso::JwksCache;
use adk_core::run_store::{DurableRunStore, InMemoryRunStore, RunFilter, RunState};
use adk_core::{
    Agent, Content, Event, EventStream, GovernanceControl, InvocationContext, Result as AdkResult,
    SingleAgentLoader,
};
use adk_runner::Runner;
use adk_server::background::DurableWorker;
use adk_server::google_triggers::{
    GoogleOidcVerifier, GooglePushIngress, PUBSUB_MESSAGE_PUBLISHED, PushTrigger,
};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use tower::ServiceExt;

const PUBSUB_AUDIENCE: &str = "https://agents.example.com/triggers/pubsub/orders";
const EVENTARC_AUDIENCE: &str = "https://agents.example.com/triggers/eventarc/uploads";
const PUSHER: &str = "orders-push@shop-prod.iam.gserviceaccount.com";
const SUBSCRIPTION: &str = "projects/shop-prod/subscriptions/orders-push";

/// Replies with the text of the user turn it received.
struct EchoAgent {
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl Agent for EchoAgent {
    fn name(&self) -> &str {
        "order_agent"
    }
    fn description(&self) -> &str {
        "echoes the event"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let text =
            ctx.user_content().parts.iter().filter_map(|part| part.text()).collect::<String>();
        let mut event = Event::new(ctx.invocation_id());
        event.author = "order_agent".to_string();
        event.llm_response.content =
            Some(Content::new("model").with_text(format!("handled: {text}")));
        Ok(Box::pin(futures::stream::iter([Ok(event)])))
    }
}

/// A JWK set served over HTTP, counting how often it is fetched.
struct Jwks {
    uri: String,
    fetches: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}

impl Jwks {
    async fn serve(keys: Vec<Value>) -> Self {
        let fetches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&fetches);
        let body = json!({ "keys": keys });
        let app = Router::new().route(
            "/oauth2/v3/certs",
            axum::routing::get(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                let body = body.clone();
                async move { axum::Json(body) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { uri: format!("http://{addr}/oauth2/v3/certs"), fetches, server }
    }
}

impl Drop for Jwks {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// A compact RS256 JWS of `claims`, signed by `signer` under its key id.
async fn mint(signer: &RsaTokenSigner, claims: &Value) -> String {
    let encode = |value: &Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap());
    let header = json!({ "alg": "RS256", "kid": signer.kid(), "typ": "JWT" });
    let input = format!("{}.{}", encode(&header), encode(claims));
    let signature = signer.sign(input.as_bytes()).await.unwrap();
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
}

/// The claims of a Pub/Sub push token: Google's issuer, the endpoint's audience, the push
/// service account.
fn claims(audience: &str) -> Value {
    let now = chrono::Utc::now().timestamp();
    json!({
        "iss": "https://accounts.google.com",
        "aud": audience,
        "azp": "104628837259046219123",
        "sub": "104628837259046219123",
        "email": PUSHER,
        "email_verified": true,
        "iat": now,
        "exp": now + 3600,
    })
}

fn pubsub_body(message_id: &str, data: &Value) -> Value {
    json!({
        "message": {
            "data": STANDARD.encode(serde_json::to_vec(data).unwrap()),
            "attributes": { "eventType": "order.created" },
            "messageId": message_id,
            "publishTime": "2026-11-18T09:00:00.123Z",
        },
        "subscription": SUBSCRIPTION,
        "deliveryAttempt": 1,
    })
}

struct Fixture {
    app: Router,
    store: Arc<InMemoryRunStore>,
    governance: GovernanceControl,
    sessions: Arc<InMemorySessionService>,
    agent: Arc<EchoAgent>,
    signer: RsaTokenSigner,
    jwks: Jwks,
}

async fn fixture() -> Fixture {
    let signer = RsaTokenSigner::generate("google-key-1").unwrap();
    let jwks = Jwks::serve(vec![serde_json::to_value(signer.public_jwk()).unwrap()]).await;
    let store = Arc::new(InMemoryRunStore::new());
    let sessions = Arc::new(InMemorySessionService::new());
    let agent = Arc::new(EchoAgent { runs: Arc::new(AtomicUsize::new(0)) });
    let governance = GovernanceControl::new();
    let config = ServerConfig::new(
        Arc::new(SingleAgentLoader::new(agent.clone() as Arc<dyn Agent>)),
        sessions.clone() as Arc<dyn SessionService>,
    )
    .with_governance(governance.clone());
    let verifier = |audience: &str| {
        GoogleOidcVerifier::new(audience, [PUSHER])
            .unwrap()
            .with_jwks_cache(JwksCache::new(jwks.uri.clone()))
    };
    let ingress = GooglePushIngress::new(store.clone() as Arc<dyn DurableRunStore>)
        .with_retry_after(Duration::from_secs(7))
        .with_trigger(PushTrigger::pubsub(
            "orders",
            verifier(PUBSUB_AUDIENCE),
            "shop",
            "order_agent",
        ))
        .with_trigger(PushTrigger::eventarc(
            "uploads",
            verifier(EVENTARC_AUDIENCE),
            "shop",
            "order_agent",
        ));
    let app = ServerBuilder::new(config).with_google_push(ingress).build();
    Fixture { app, store, governance, sessions, agent, signer, jwks }
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value, Option<String>) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), retry_after)
}

fn push(path: &str, token: Option<&str>, body: &Value) -> Request<Body> {
    let mut request = Request::post(path).header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    request.body(Body::from(serde_json::to_vec(body).unwrap())).unwrap()
}

async fn runs(fixture: &Fixture) -> Vec<adk_core::run_store::RunRecord> {
    fixture.store.list(RunFilter::default()).await.unwrap()
}

async fn run_queued(fixture: &Fixture) -> usize {
    let runner = Runner::builder()
        .app_name("shop")
        .agent(fixture.agent.clone() as Arc<dyn Agent>)
        .session_service(fixture.sessions.clone() as Arc<dyn SessionService>)
        .build()
        .unwrap();
    let worker = DurableWorker::new(fixture.store.clone() as Arc<dyn DurableRunStore>)
        .with_agent("order_agent", Arc::new(runner))
        .with_governance(fixture.governance.clone());
    let mut finished = 0;
    while worker.run_next().await.unwrap().is_some() {
        finished += 1;
    }
    finished
}

/// A verified push is acknowledged once its run is enqueued; Pub/Sub's redelivery of the same
/// message is acknowledged with the same run, and the agent runs once. The key set is fetched
/// once for both.
#[tokio::test]
async fn a_signed_pubsub_push_runs_once_despite_a_redelivery() {
    let fixture = fixture().await;
    let token = mint(&fixture.signer, &claims(PUBSUB_AUDIENCE)).await;
    let body = pubsub_body("136969346945", &json!({ "order": 7, "total": "12.50" }));

    let (status, first, _) =
        send(&fixture.app, push("/triggers/pubsub/orders", Some(&token), &body)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    assert_eq!(first["duplicate"], false);
    assert_eq!(first["key"], format!("pubsub:orders:{SUBSCRIPTION}:136969346945"));
    let (status, again, _) =
        send(&fixture.app, push("/triggers/pubsub/orders", Some(&token), &body)).await;
    assert_eq!((status, again["duplicate"].as_bool()), (StatusCode::OK, Some(true)));
    assert_eq!(again["runId"], first["runId"]);
    assert_eq!(fixture.jwks.fetches.load(Ordering::SeqCst), 1, "the key set is cached");

    let stored = runs(&fixture).await;
    assert_eq!(stored.len(), 1);
    let run = &stored[0];
    assert_eq!(
        (run.app_name.as_str(), run.target.as_str(), run.user_id.as_str()),
        ("shop", "order_agent", "pubsub:orders")
    );
    assert!(!run.authenticated, "the run acts as a service, not as an authenticated user");
    let event = &run.input["pushEvent"];
    assert_eq!(event["data"], json!({ "order": 7, "total": "12.50" }));
    assert_eq!(event["attributes"]["eventType"], "order.created");
    assert_eq!(event["sender"]["email"], PUSHER);
    let text = run.input["text"].as_str().unwrap();
    assert!(text.starts_with("[pubsub \"136969346945\"]\n"), "{text}");
    assert!(text.contains("not instructions"), "{text}");

    assert_eq!(run_queued(&fixture).await, 1);
    let (status, third, _) =
        send(&fixture.app, push("/triggers/pubsub/orders", Some(&token), &body)).await;
    assert_eq!((status, third["state"].as_str()), (StatusCode::OK, Some("completed")));
    assert_eq!(run_queued(&fixture).await, 0, "a redelivery after the run starts nothing");
    assert_eq!(fixture.agent.runs.load(Ordering::SeqCst), 1);
}

/// Every token check refuses before anything is enqueued: `401` for a token that does not
/// authenticate, `403` for one from an account the trigger does not allow.
#[tokio::test]
async fn tokens_that_do_not_verify_are_refused() {
    let fixture = fixture().await;
    let body = pubsub_body("m-1", &json!({ "order": 1 }));
    let with = |change: &dyn Fn(&mut Value)| {
        let mut claims = claims(PUBSUB_AUDIENCE);
        change(&mut claims);
        claims
    };
    let impostor = RsaTokenSigner::generate("google-key-1").unwrap();
    let cases: Vec<(&str, Option<String>, StatusCode)> = vec![
        ("no token", None, StatusCode::UNAUTHORIZED),
        ("not a JWT", Some("not-a-jwt".into()), StatusCode::UNAUTHORIZED),
        (
            "another endpoint's audience",
            Some(mint(&fixture.signer, &claims(EVENTARC_AUDIENCE)).await),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "another issuer",
            Some(
                mint(&fixture.signer, &with(&|c| c["iss"] = json!("https://evil.example.com")))
                    .await,
            ),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "expired",
            Some(
                mint(
                    &fixture.signer,
                    &with(&|c| {
                        c["exp"] = json!(chrono::Utc::now().timestamp() - 600);
                        c["iat"] = json!(chrono::Utc::now().timestamp() - 4200);
                    }),
                )
                .await,
            ),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "signed by another key under Google's key id",
            Some(mint(&impostor, &claims(PUBSUB_AUDIENCE)).await),
            StatusCode::UNAUTHORIZED,
        ),
        (
            "an account the trigger does not allow",
            Some(mint(&fixture.signer, &with(&|c| c["email"] = json!("someone@gmail.com"))).await),
            StatusCode::FORBIDDEN,
        ),
        (
            "an unverified email",
            Some(mint(&fixture.signer, &with(&|c| c["email_verified"] = json!(false))).await),
            StatusCode::FORBIDDEN,
        ),
    ];
    for (case, token, expected) in cases {
        let (status, body, _) =
            send(&fixture.app, push("/triggers/pubsub/orders", token.as_deref(), &body)).await;
        assert_eq!(status, expected, "{case}: {body}");
    }
    assert!(runs(&fixture).await.is_empty(), "a refused delivery enqueues nothing");
}

/// While the kill switch is on, a delivery is refused with `503` and `Retry-After`, so Pub/Sub
/// redelivers it; once lifted, the redelivery runs.
#[tokio::test]
async fn a_frozen_server_refuses_deliveries_until_the_freeze_lifts() {
    let fixture = fixture().await;
    let token = mint(&fixture.signer, &claims(PUBSUB_AUDIENCE)).await;
    let body = pubsub_body("m-frozen", &json!({ "order": 2 }));
    fixture.governance.freeze("incident");
    let (status, _, retry_after) =
        send(&fixture.app, push("/triggers/pubsub/orders", Some(&token), &body)).await;
    assert_eq!((status, retry_after.as_deref()), (StatusCode::SERVICE_UNAVAILABLE, Some("7")));
    assert!(runs(&fixture).await.is_empty());
    fixture.governance.unfreeze();
    let (status, _, _) =
        send(&fixture.app, push("/triggers/pubsub/orders", Some(&token), &body)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

/// Eventarc delivers a CloudEvent in binary mode (`ce-*` headers) or structured mode; each is
/// keyed by its source and id, and a Pub/Sub `messagePublished` event's message is unwrapped.
#[tokio::test]
async fn eventarc_events_start_runs_keyed_by_source_and_id() {
    let fixture = fixture().await;
    let token = mint(&fixture.signer, &claims(EVENTARC_AUDIENCE)).await;
    let source = "//storage.googleapis.com/projects/_/buckets/uploads";
    let binary = |id: &str| {
        Request::post("/triggers/eventarc/uploads")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .header("ce-specversion", "1.0")
            .header("ce-id", id)
            .header("ce-source", source)
            .header("ce-type", "google.cloud.storage.object.v1.finalized")
            .header("ce-subject", "objects/cat.png")
            .body(Body::from(r#"{"name":"cat.png","bucket":"uploads"}"#))
            .unwrap()
    };
    let (status, first, _) = send(&fixture.app, binary("evt-1")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    assert_eq!(first["key"], format!("eventarc:uploads:{source}:evt-1"));
    let (status, again, _) = send(&fixture.app, binary("evt-1")).await;
    assert_eq!((status, again["duplicate"].as_bool()), (StatusCode::OK, Some(true)));

    let published = json!({
        "specversion": "1.0",
        "id": "evt-2",
        "source": "//pubsub.googleapis.com/projects/shop-prod/topics/orders",
        "type": PUBSUB_MESSAGE_PUBLISHED,
        "data": pubsub_body("m-77", &json!({ "order": 77 })),
    });
    let structured = Request::post("/triggers/eventarc/uploads")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/cloudevents+json")
        .body(Body::from(published.to_string()))
        .unwrap();
    let (status, _, _) = send(&fixture.app, structured).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let stored = runs(&fixture).await;
    let event = stored
        .iter()
        .map(|run| &run.input["pushEvent"])
        .find(|event| event["id"] == "evt-2")
        .unwrap();
    assert_eq!(event["data"], json!({ "order": 77 }), "the Pub/Sub message is unwrapped");
    assert_eq!(event["eventType"], PUBSUB_MESSAGE_PUBLISHED);
    assert_eq!(stored.len(), 2);

    let missing_id = Request::post("/triggers/eventarc/uploads")
        .header("authorization", format!("Bearer {token}"))
        .header("ce-source", source)
        .header("ce-type", "google.cloud.storage.object.v1.finalized")
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(send(&fixture.app, missing_id).await.0, StatusCode::BAD_REQUEST);
    let wrong_kind = push("/triggers/eventarc/orders", Some(&token), &json!({}));
    assert_eq!(send(&fixture.app, wrong_kind).await.0, StatusCode::NOT_FOUND);
    let wrong_audience =
        push("/triggers/pubsub/orders", Some(&token), &pubsub_body("m-2", &json!({})));
    assert_eq!(send(&fixture.app, wrong_audience).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(runs(&fixture).await.len(), 2);
}

#[test]
fn a_trigger_needs_an_audience_and_an_allowlist() {
    use adk_server::google_triggers::PushTriggerError;
    assert!(matches!(
        GoogleOidcVerifier::new(PUBSUB_AUDIENCE, Vec::<String>::new()),
        Err(PushTriggerError::NoAllowedEmails)
    ));
    assert!(matches!(GoogleOidcVerifier::new(" ", [PUSHER]), Err(PushTriggerError::NoAudience)));
}

/// LIVE on Anthropic `claude-haiku-5-5`: a simulated Pub/Sub push, signed with a local key
/// against a local JWK set, runs an agent once although Pub/Sub delivers it twice.
///
/// ```text
/// cargo test -p adk-server --features google-triggers,background --test google_trigger_tests live_ -- --ignored --nocapture
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: needs ANTHROPIC_API_KEY"]
async fn live_a_pubsub_push_runs_an_agent_once_despite_a_redelivery() {
    let _ = dotenvy::from_path(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env"));
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    let model: Arc<dyn adk_core::Llm> = Arc::new(
        adk_model::anthropic::AnthropicClient::new(adk_model::anthropic::AnthropicConfig::new(
            key,
            "claude-haiku-5-5",
        ))
        .unwrap(),
    );
    let agent = adk_agent::LlmAgentBuilder::new("order_agent")
        .instruction("You handle order events. Reply with one short sentence naming the order number and its total.")
        .model(model)
        .build()
        .unwrap();
    let signer = RsaTokenSigner::generate("google-key-live").unwrap();
    let jwks = Jwks::serve(vec![serde_json::to_value(signer.public_jwk()).unwrap()]).await;
    let store = Arc::new(InMemoryRunStore::new());
    let sessions = Arc::new(InMemorySessionService::new());
    let agent: Arc<dyn Agent> = Arc::new(agent);
    let config = ServerConfig::new(
        Arc::new(SingleAgentLoader::new(Arc::clone(&agent))),
        sessions.clone() as Arc<dyn SessionService>,
    );
    let verifier = GoogleOidcVerifier::new(PUBSUB_AUDIENCE, [PUSHER])
        .unwrap()
        .with_jwks_cache(JwksCache::new(jwks.uri.clone()));
    let ingress = GooglePushIngress::new(store.clone() as Arc<dyn DurableRunStore>)
        .with_trigger(PushTrigger::pubsub("orders", verifier, "shop", "order_agent"));
    let app = ServerBuilder::new(config).with_google_push(ingress).build();
    let runner = Runner::builder()
        .app_name("shop")
        .agent(agent)
        .session_service(sessions as Arc<dyn SessionService>)
        .build()
        .unwrap();
    let worker = DurableWorker::new(store.clone() as Arc<dyn DurableRunStore>)
        .with_agent("order_agent", Arc::new(runner));

    let token = mint(&signer, &claims(PUBSUB_AUDIENCE)).await;
    let body = pubsub_body("live-136969346945", &json!({ "order": 4471, "total": "89.90 EUR" }));
    let (status, first, _) = send(&app, push("/triggers/pubsub/orders", Some(&token), &body)).await;
    println!("first delivery: {status} {first}");
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, second, _) =
        send(&app, push("/triggers/pubsub/orders", Some(&token), &body)).await;
    println!("redelivery: {status} {second}");
    assert_eq!((status, second["duplicate"].as_bool()), (StatusCode::OK, Some(true)));

    let mut ran = 0;
    while worker.run_next().await.unwrap().is_some() {
        ran += 1;
    }
    let (status, third, _) = send(&app, push("/triggers/pubsub/orders", Some(&token), &body)).await;
    println!("redelivery after the run: {status} {third}");
    while worker.run_next().await.unwrap().is_some() {
        ran += 1;
    }
    let stored = store.list(RunFilter::default()).await.unwrap();
    assert_eq!((ran, stored.len()), (1, 1), "one run for three deliveries");
    let run = &stored[0];
    println!("run {} {:?}: {:?}", run.run_id, run.state, run.result);
    assert_eq!(run.state, RunState::Completed);
}
