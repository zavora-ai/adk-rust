//! Outbound A2A calls authenticated through a `CredentialBroker` send the brokered
//! token only to the resource it was minted for.

use adk_core::{
    ActingContext, AgentId, Content, CredentialBroker, Result, RevocationScope, ScopedToken, Url,
    async_trait,
};
use adk_runner::Runner;
use adk_server::a2a::{A2aClient, Message, Part, RemoteA2aAgent, Role};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// The `Authorization` header of every message request a server received.
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<String>>>);

impl Seen {
    fn all(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

async fn message(
    State(seen): State<Seen>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    seen.0.lock().unwrap().push(auth);
    Json(json!({
        "jsonrpc": "2.0",
        "id": body["id"],
        "result": {
            "id": "task-1",
            "contextId": "ctx-1",
            "status": { "state": "completed" },
            "kind": "task",
        },
    }))
}

/// Serves an agent card whose endpoint is `endpoint` (or this server's `/a2a` when
/// `None`) and records message requests. Returns the base URL.
async fn start(seen: Seen, endpoint: Option<String>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let card = json!({
        "name": "remote",
        "description": "credential broker fixture",
        "url": endpoint.unwrap_or_else(|| format!("{base}/a2a")),
        "version": "1.0.0",
    });
    let app = Router::new()
        .route("/.well-known/agent-card.json", get(move || async move { Json(card) }))
        .route("/a2a", post(message))
        .with_state(seen);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

/// Issues `token-for:<audience>`, with the requested resource as the audience unless
/// `audience` overrides it, and records who asked for what.
#[derive(Debug, Default)]
struct MockBroker {
    audience: Option<Url>,
    requests: Mutex<Vec<(String, String, Vec<String>)>>,
}

#[async_trait]
impl CredentialBroker for MockBroker {
    async fn token_for(
        &self,
        acting: &ActingContext,
        resource: &Url,
        scopes: &[String],
    ) -> Result<ScopedToken> {
        self.requests.lock().unwrap().push((
            acting.agent.id.to_string(),
            resource.to_string(),
            scopes.to_vec(),
        ));
        let audience = self.audience.clone().unwrap_or_else(|| resource.clone());
        Ok(ScopedToken::new(
            acting.agent.id.clone(),
            format!("token-for:{audience}"),
            audience,
            scopes.to_vec(),
        ))
    }

    async fn revoke(&self, _scope: &RevocationScope) -> Result<u64> {
        Ok(0)
    }
}

fn token(audience: &str) -> ScopedToken {
    ScopedToken::new(
        AgentId::new("urn:adk:agent:caller").unwrap(),
        "secret",
        adk_core::resource_indicator(audience).unwrap(),
        vec![],
    )
}

fn hello() -> Message {
    Message::builder()
        .role(Role::User)
        .parts(vec![Part::text("hello".to_string())])
        .message_id("m-1".to_string())
        .build()
}

#[tokio::test]
async fn the_client_attaches_a_token_only_inside_its_audience() {
    let seen = Seen::default();
    let base = start(seen.clone(), None).await;

    let client = A2aClient::from_url(&base).await.unwrap().with_credential(token(&base));
    client.send_message(hello()).await.unwrap();
    assert_eq!(seen.all(), vec!["Bearer secret".to_string()]);

    let client = A2aClient::from_url(&base)
        .await
        .unwrap()
        .with_credential(token("https://other.example.test/"));
    let error = client.send_message(hello()).await.unwrap_err();
    assert_eq!(error.code, "credential.audience_mismatch");
    assert_eq!(seen.all().len(), 1, "the refused request was not sent");
}

async fn run_remote(agent: RemoteA2aAgent) -> Vec<String> {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "ops".into(),
            user_id: "alice".into(),
            session_id: Some("s-1".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("ops")
        .agent(Arc::new(agent))
        .session_service(sessions)
        .build()
        .unwrap();
    let mut events =
        runner.run_str("alice", "s-1", Content::new("user").with_text("hello")).await.unwrap();
    let mut texts = Vec::new();
    while let Some(event) = events.next().await {
        let event = event.unwrap();
        if let Some(content) = event.llm_response.content {
            texts.extend(content.parts.iter().filter_map(|part| part.text().map(str::to_string)));
        }
        if let Some(message) = event.llm_response.error_message {
            texts.push(message);
        }
    }
    texts
}

#[tokio::test]
async fn the_remote_agent_mints_for_the_running_agent_and_its_url() {
    let seen = Seen::default();
    let base = start(seen.clone(), None).await;
    let broker = Arc::new(MockBroker::default());

    let agent = RemoteA2aAgent::builder("remote")
        .agent_url(&base)
        .streaming(false)
        .credential_broker(broker.clone(), vec!["a2a.message".to_string()])
        .build()
        .unwrap();
    run_remote(agent).await;

    let resource = Url::parse(&base).unwrap().to_string();
    assert_eq!(
        broker.requests.lock().unwrap().clone(),
        vec![(
            "urn:adk:agent:ops/remote".to_string(),
            resource.clone(),
            vec!["a2a.message".to_string()]
        )]
    );
    assert_eq!(seen.all(), vec![format!("Bearer token-for:{resource}")]);
}

#[tokio::test]
async fn a_card_pointing_elsewhere_never_receives_the_token() {
    // The card served by `trusted` names an endpoint on another server.
    let elsewhere = Seen::default();
    let other = start(elsewhere.clone(), None).await;
    let trusted = Seen::default();
    let base = start(trusted.clone(), Some(format!("{other}/a2a"))).await;

    let agent = RemoteA2aAgent::builder("remote")
        .agent_url(&base)
        .streaming(false)
        .credential_broker(Arc::new(MockBroker::default()), vec![])
        .build()
        .unwrap();
    let output = run_remote(agent).await;

    assert!(elsewhere.all().is_empty(), "no request reached the card's endpoint");
    assert!(trusted.all().is_empty());
    assert!(output.iter().any(|text| text.contains("must not be sent")), "{output:?}");
}
