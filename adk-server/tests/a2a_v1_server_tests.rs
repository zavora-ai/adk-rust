//! Serving A2A v1 in one call: `A2aV1Server` and `ServerBuilder::with_a2a`, as a v1 peer, a
//! 0.3 peer and adk-server's own clients see them (zavora-ai/adk-rust#1343).
#![cfg(feature = "a2a-v1")]

use std::sync::Arc;

use a2a_protocol_types::responses::SendMessageResponse;
use a2a_protocol_types::task::TaskState;
use a2a_protocol_types::{
    AgentCapabilities, AgentInterface, Message, MessageId, MessageRole, Part,
};
use adk_core::SingleAgentLoader;
use adk_core::{Agent, Content, Event, EventStream, InvocationContext, Result as AdkResult};
use adk_server::a2a::client::v1_client::{A2aV1Client, v1_interface};
use adk_server::a2a::v1::card::build_v1_agent_card;
use adk_server::a2a::v1::server::A2aV1Server;
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use axum::Router;
use serde_json::{Value, json};

const ANSWER: &str = "It is sunny.";

/// Answers every message with [`ANSWER`].
struct WeatherAgent;

#[async_trait]
impl Agent for WeatherAgent {
    fn name(&self) -> &str {
        "weather"
    }

    fn description(&self) -> &str {
        "tells the weather"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        let mut event = Event::new("weather");
        event.llm_response.content = Some(Content::new("model").with_text(ANSWER));
        Ok(Box::pin(futures::stream::iter(vec![Ok(event)])))
    }
}

/// Serves the router `build` makes for the listener's base URL; returns that URL.
async fn listen(build: impl FnOnce(&str) -> Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = build(&base);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

/// The app `ServerBuilder::with_a2a` builds for [`WeatherAgent`].
async fn serve_with_a2a() -> String {
    listen(|base| {
        let config = ServerConfig::new(
            Arc::new(SingleAgentLoader::new(Arc::new(WeatherAgent))),
            Arc::new(InMemorySessionService::new()),
        );
        ServerBuilder::new(config).with_a2a(base).build()
    })
    .await
}

fn user_message(text: &str) -> Message {
    Message {
        id: MessageId::new(uuid::Uuid::new_v4().to_string()),
        role: MessageRole::User,
        parts: vec![Part::text(text)],
        task_id: None,
        context_id: None,
        reference_task_ids: None,
        extensions: None,
        metadata: None,
    }
}

fn artifact_texts(task: &a2a_protocol_types::Task) -> Vec<String> {
    task.artifacts
        .iter()
        .flatten()
        .flat_map(|artifact| &artifact.parts)
        .filter_map(|part| match &part.content {
            a2a_protocol_types::PartContent::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn with_a2a_serves_a_v1_card_that_also_names_the_0_3_endpoint() {
    let base = serve_with_a2a().await;
    let http = reqwest::Client::new();

    let response = http.get(format!("{base}/.well-known/agent-card.json")).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.headers().contains_key("etag"), "the card is served with an ETag");
    let card: Value = response.json().await.unwrap();
    assert_eq!(
        card["supportedInterfaces"],
        json!([
            {"url": format!("{base}/a2a/v1"), "protocolBinding": "JSONRPC", "protocolVersion": "1.0"},
            {"url": format!("{base}/a2a/v1/rest"), "protocolBinding": "HTTP+JSON", "protocolVersion": "1.0"},
            {"url": format!("{base}/a2a"), "protocolBinding": "JSONRPC", "protocolVersion": "0.3"},
        ])
    );
    assert!(card.get("url").is_none(), "a v1 card has no top-level url (A2A v1 schema)");
    assert_eq!(card["name"], "weather");

    // adk-server's 0.3 client passes over the v1 card for the 0.3 one, which points it at
    // `/a2a`.
    let legacy = adk_server::a2a::A2aClient::resolve_agent_card(&base).await.unwrap();
    assert_eq!(legacy.url, format!("{base}/a2a"));

    // The 0.3 card keeps its own path.
    let legacy_card: Value = http
        .get(format!("{base}/.well-known/agent.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(legacy_card["url"], format!("{base}/a2a"));
}

#[tokio::test]
async fn with_a2a_answers_v1_on_both_bindings_and_0_3_on_its_endpoint() {
    let base = serve_with_a2a().await;

    // adk-server's v1 client, from the card alone.
    let card = A2aV1Client::resolve_agent_card(&base).await.unwrap();
    let client = A2aV1Client::new(card);
    match client.send_message(user_message("weather?")).await.unwrap() {
        SendMessageResponse::Task(task) => {
            assert_eq!(task.status.state, TaskState::Completed);
            assert_eq!(artifact_texts(&task), vec![ANSWER.to_string()]);
        }
        other => panic!("expected a task, got {other:?}"),
    }
    match client.send_message_rest(user_message("weather?")).await.unwrap() {
        SendMessageResponse::Task(task) => assert_eq!(task.status.state, TaskState::Completed),
        other => panic!("expected a task, got {other:?}"),
    }

    // A 0.3 peer still reaches the 0.3 endpoint.
    let reply: Value = reqwest::Client::new()
        .post(format!("{base}/a2a"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "message/send",
            "params": {"message": {
                "kind": "message",
                "messageId": "m-1",
                "role": "user",
                "parts": [{"kind": "text", "text": "weather?"}]
            }}
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(reply.get("result").is_some(), "the 0.3 endpoint answers: {reply}");
}

#[tokio::test]
async fn a2a_v1_server_mounts_its_bindings_where_its_card_says() {
    let base = listen(|base| {
        let runner_config = Arc::new(
            adk_runner::Runner::builder()
                .app_name("weather")
                .agent(Arc::new(WeatherAgent))
                .session_service(Arc::new(InMemorySessionService::new()))
                .build_config(),
        );
        A2aV1Server::builder(runner_config, base)
            .jsonrpc_path("/rpc")
            .rest_path("/v1")
            .version("2.1.0")
            .build()
            .router()
    })
    .await;

    let card = A2aV1Client::resolve_agent_card(&base).await.unwrap();
    assert_eq!(card.version, "2.1.0");
    let urls: Vec<_> = card.supported_interfaces.iter().map(|i| i.url.clone()).collect();
    assert_eq!(urls, vec![format!("{base}/rpc"), format!("{base}/v1")]);
    assert_eq!(card.capabilities.streaming, Some(true));
    assert_ne!(
        card.capabilities.push_notifications,
        Some(true),
        "push is declared only with a sender"
    );
    assert!(!card.skills.is_empty(), "skills come from the agent");

    let client = A2aV1Client::new(card);
    assert!(matches!(
        client.send_message_rest(user_message("weather?")).await.unwrap(),
        SendMessageResponse::Task(task) if task.status.state == TaskState::Completed
    ));
}

#[test]
fn a_client_never_picks_an_interface_for_an_earlier_protocol_version() {
    let mut card = build_v1_agent_card(
        "agent",
        "an agent",
        "http://h/a2a/v1",
        "1.0.0",
        vec![],
        AgentCapabilities::none(),
    );
    let interface = |url: &str, version: &str| AgentInterface {
        url: url.to_string(),
        protocol_binding: "JSONRPC".to_string(),
        protocol_version: version.to_string(),
        tenant: None,
    };

    // A 0.3 endpoint listed first is passed over for the v1 one.
    card.supported_interfaces =
        vec![interface("http://h/a2a", "0.3"), interface("http://h/a2a/v1", "1.0")];
    assert_eq!(v1_interface(&card, "JSONRPC").unwrap().url, "http://h/a2a/v1");

    // An interface that states no version is taken when no 1.x one exists.
    card.supported_interfaces =
        vec![interface("http://h/a2a", "0.3"), interface("http://h/any", "")];
    assert_eq!(v1_interface(&card, "JSONRPC").unwrap().url, "http://h/any");

    // A card that only offers 0.3 has no v1 interface.
    card.supported_interfaces = vec![interface("http://h/a2a", "0.3.0")];
    assert!(v1_interface(&card, "JSONRPC").is_none());
}
