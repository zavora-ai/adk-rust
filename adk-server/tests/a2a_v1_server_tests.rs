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
    assert_eq!(
        card.capabilities.extensions.as_ref().map(|extensions| extensions[0].uri.as_str()),
        Some(adk_server::a2a::v1::convert::ADK_EXTENSION_URI),
        "the card declares the ADK extension"
    );

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

/// A runner-backed v1 server for [`WeatherAgent`], optionally serving only its HTTP+JSON
/// binding on the card.
async fn serve_weather(rest_only: bool) -> String {
    listen(|base| {
        let runner_config = Arc::new(
            adk_runner::Runner::builder()
                .app_name("weather")
                .agent(Arc::new(WeatherAgent))
                .session_service(Arc::new(InMemorySessionService::new()))
                .build_config(),
        );
        let server = A2aV1Server::builder(runner_config, base).build();
        if rest_only {
            let card = server.card().clone();
            let mut cached = card.try_write().unwrap();
            let mut rest = cached.card.clone();
            rest.supported_interfaces.retain(|i| i.protocol_binding == "HTTP+JSON");
            cached.update(rest);
        }
        server.router()
    })
    .await
}

fn texts(updates: &[a2a_protocol_types::events::StreamResponse]) -> String {
    updates
        .iter()
        .filter_map(|update| match update {
            a2a_protocol_types::events::StreamResponse::ArtifactUpdate(event) => {
                Some(&event.artifact)
            }
            _ => None,
        })
        .flat_map(|artifact| &artifact.parts)
        .filter_map(|part| part.text_content().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn the_client_streams_parsed_updates_over_either_binding() {
    use futures::StreamExt;

    for rest_only in [false, true] {
        let base = serve_weather(rest_only).await;
        let client = A2aV1Client::new(A2aV1Client::resolve_agent_card(&base).await.unwrap());
        let updates: Vec<_> = client
            .stream_message(user_message("weather?"))
            .await
            .unwrap()
            .map(|update| update.unwrap())
            .collect()
            .await;
        assert_eq!(texts(&updates), ANSWER, "rest_only={rest_only}");
        assert!(matches!(
            updates.last(),
            Some(a2a_protocol_types::events::StreamResponse::StatusUpdate(event))
                if event.status.state == TaskState::Completed
        ));
    }
}

#[tokio::test]
async fn the_client_subscribes_to_a_task_it_started_with_return_immediately() {
    use futures::StreamExt;

    let base = serve_weather(false).await;
    let client = A2aV1Client::new(A2aV1Client::resolve_agent_card(&base).await.unwrap());
    let configuration: a2a_protocol_types::params::SendMessageConfiguration =
        serde_json::from_value(json!({"returnImmediately": true})).unwrap();
    let task = match client
        .send_message_with(user_message("weather?"), Some(&configuration))
        .await
        .unwrap()
    {
        SendMessageResponse::Task(task) => task,
        other => panic!("expected a task, got {other:?}"),
    };
    // The run may already be over: then the task is finished, and subscribing is refused,
    // as the stream's one event (an a2a-go client reads only `data:` lines).
    let refused = |error: &adk_server::a2a::client::v1_client::V1ClientError| {
        matches!(
            error,
            adk_server::a2a::client::v1_client::V1ClientError::JsonRpc { code: -32004, .. }
        )
    };
    match client.subscribe(&task.id.0).await {
        Ok(updates) => {
            let updates: Vec<_> = updates.collect().await;
            assert!(!updates.is_empty());
            match &updates[0] {
                Ok(_) => assert!(updates.iter().all(Result::is_ok), "{updates:?}"),
                Err(error) => assert!(refused(error) && updates.len() == 1, "{updates:?}"),
            }
        }
        Err(error) => assert!(refused(&error), "unexpected error: {error}"),
    }
    let done = client.get_task(&task.id.0, None).await.unwrap();
    assert_eq!(done.status.state, TaskState::Completed);
}

#[tokio::test]
async fn a_remote_agent_from_a_url_continues_one_remote_conversation() {
    use adk_session::{CreateRequest, SessionService};
    use futures::StreamExt;

    let base = serve_weather(false).await;
    let remote = adk_server::a2a::remote_agent::v1_remote::RemoteA2aV1Agent::from_url(
        "weather_remote",
        &base,
    )
    .await
    .unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "caller".to_string(),
            user_id: "u1".to_string(),
            session_id: Some("s1".to_string()),
            state: std::collections::HashMap::new(),
        })
        .await
        .unwrap();
    let runner = adk_runner::Runner::builder()
        .app_name("caller")
        .agent(Arc::new(remote))
        .session_service(sessions)
        .build()
        .unwrap();

    for turn in ["weather today?", "and tomorrow?"] {
        let events: Vec<_> = runner
            .run(
                adk_core::UserId::new("u1").unwrap(),
                adk_core::SessionId::new("s1").unwrap(),
                Content::new("user").with_text(turn),
            )
            .await
            .unwrap()
            .map(|event| event.unwrap())
            .collect()
            .await;
        let said: String = events
            .iter()
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| &content.parts)
            .filter_map(|part| part.text())
            .collect();
        assert!(said.contains(ANSWER), "turn {turn:?}: {said}");
    }

    // Both turns ran in one remote context.
    let client = A2aV1Client::new(A2aV1Client::resolve_agent_card(&base).await.unwrap());
    let tasks = client.list_tasks(None, None, None, None).await.unwrap().tasks;
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].context_id, tasks[1].context_id, "the second turn continued the first");
}

#[tokio::test]
async fn a_streaming_methods_early_error_is_the_streams_one_event() {
    let base = serve_weather(false).await;
    let card = A2aV1Client::resolve_agent_card(&base).await.unwrap();
    let url = card
        .supported_interfaces
        .iter()
        .find(|interface| interface.protocol_binding == "JSONRPC")
        .unwrap()
        .url
        .clone();
    let response = reqwest::Client::new()
        .post(&url)
        .header("A2A-Version", "1.0")
        .json(&json!({"jsonrpc": "2.0", "id": 7, "method": "SubscribeToTask", "params": {"id": "no-such-task"}}))
        .send()
        .await
        .unwrap();

    // As a2a-go answers: SSE, with the JSON-RPC error as the `data:` event, so a client that
    // reads only `data:` lines from a streaming method sees it.
    assert_eq!(response.status(), 200);
    let content_type = response.headers()["content-type"].to_str().unwrap().to_string();
    assert!(content_type.starts_with("text/event-stream"), "{content_type}");
    let body = response.text().await.unwrap();
    let data = body.lines().find_map(|line| line.strip_prefix("data:")).expect("a data line");
    let error: serde_json::Value = serde_json::from_str(data.trim()).unwrap();
    assert_eq!(error["id"], 7);
    assert_eq!(error["error"]["code"], -32001);
}

/// Says it started, then takes a while to answer.
struct SlowAgent;

#[async_trait]
impl Agent for SlowAgent {
    fn name(&self) -> &str {
        "slow"
    }

    fn description(&self) -> &str {
        "answers slowly"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        Ok(Box::pin(async_stream::stream! {
            let mut started = Event::new("slow");
            started.llm_response.content = Some(Content::new("model").with_text("Working on it."));
            yield Ok(started);
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            let mut done = Event::new("slow");
            done.llm_response.content = Some(Content::new("model").with_text("Done."));
            yield Ok(done);
        }))
    }
}

#[tokio::test]
async fn a_remote_agent_cancels_the_remote_task_when_its_caller_stops_reading() {
    use adk_session::{CreateRequest, SessionService};
    use futures::StreamExt;

    let base = listen(|base| {
        let runner_config = Arc::new(
            adk_runner::Runner::builder()
                .app_name("slow")
                .agent(Arc::new(SlowAgent))
                .session_service(Arc::new(InMemorySessionService::new()))
                .build_config(),
        );
        A2aV1Server::builder(runner_config, base).build().router()
    })
    .await;
    let remote =
        adk_server::a2a::remote_agent::v1_remote::RemoteA2aV1Agent::from_url("slow_remote", &base)
            .await
            .unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "caller".to_string(),
            user_id: "u1".to_string(),
            session_id: Some("s1".to_string()),
            state: std::collections::HashMap::new(),
        })
        .await
        .unwrap();
    let runner = adk_runner::Runner::builder()
        .app_name("caller")
        .agent(Arc::new(remote))
        .session_service(sessions)
        .build()
        .unwrap();

    let mut events = runner
        .run(
            adk_core::UserId::new("u1").unwrap(),
            adk_core::SessionId::new("s1").unwrap(),
            Content::new("user").with_text("start"),
        )
        .await
        .unwrap();
    let first = events.next().await.expect("the remote's first update").unwrap();
    assert!(first.llm_response.content.is_some());
    drop(events);

    // As adk-go's remote agent: the remote task is cancelled, not left running.
    let client = A2aV1Client::new(A2aV1Client::resolve_agent_card(&base).await.unwrap());
    let mut state = None;
    for _ in 0..100 {
        let tasks = client.list_tasks(None, None, None, None).await.unwrap().tasks;
        state = tasks.first().map(|task| task.status.state);
        if state == Some(TaskState::Canceled) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(state, Some(TaskState::Canceled));
}

/// Asks its caller to approve with a long-running call.
struct AskingAgent;

#[async_trait]
impl Agent for AskingAgent {
    fn name(&self) -> &str {
        "asking"
    }

    fn description(&self) -> &str {
        "asks for approval"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        let mut event = Event::new("asking");
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![adk_core::Part::FunctionCall {
                name: "approve".to_string(),
                args: json!({"amount": 5}),
                id: Some("call-1".to_string()),
                thought_signature: None,
            }],
        });
        event.long_running_tool_ids = vec!["call-1".to_string()];
        Ok(Box::pin(futures::stream::iter(vec![Ok(event)])))
    }
}

/// Serves `agent` over A2A v1 at a new address; returns the address.
async fn serve_agent(agent: Arc<dyn Agent>) -> String {
    listen(|base| {
        let runner_config = Arc::new(
            adk_runner::Runner::builder()
                .app_name(agent.name())
                .agent(agent.clone())
                .session_service(Arc::new(InMemorySessionService::new()))
                .build_config(),
        );
        A2aV1Server::builder(runner_config, base).build().router()
    })
    .await
}

#[tokio::test]
async fn cancelling_a_task_cancels_the_remote_task_it_waits_on() {
    let child = serve_agent(Arc::new(AskingAgent)).await;
    let remote =
        adk_server::a2a::remote_agent::v1_remote::RemoteA2aV1Agent::from_url("approver", &child)
            .await
            .unwrap();
    let parent = serve_agent(Arc::new(remote)).await;
    let parent_client = A2aV1Client::new(A2aV1Client::resolve_agent_card(&parent).await.unwrap());
    let child_client = A2aV1Client::new(A2aV1Client::resolve_agent_card(&child).await.unwrap());

    let task = match parent_client.send_message(user_message("pay 5")).await.unwrap() {
        SendMessageResponse::Task(task) => task,
        other => panic!("expected a task, got {other:?}"),
    };
    assert_eq!(task.status.state, TaskState::InputRequired, "the parent waits on the child's call");
    let child_tasks = child_client.list_tasks(None, None, None, None).await.unwrap().tasks;
    assert_eq!(child_tasks[0].status.state, TaskState::InputRequired);

    parent_client.cancel_task(&task.id.0).await.unwrap();

    // As adk-go's executor: the child's waiting task is cancelled too.
    let child_task = child_client.get_task(&child_tasks[0].id.0, None).await.unwrap();
    assert_eq!(child_task.status.state, TaskState::Canceled);
}

#[tokio::test]
async fn a_get_to_the_json_rpc_endpoint_is_a_json_rpc_error() {
    let base = serve_weather(false).await;
    let card = A2aV1Client::resolve_agent_card(&base).await.unwrap();
    let url = v1_interface(&card, "JSONRPC").unwrap().url.clone();
    let response =
        reqwest::Client::new().get(&url).header("A2A-Version", "1.0").send().await.unwrap();
    // As a2a-go: a JSON-RPC invalid request, not a bare 405.
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], -32600);
}
