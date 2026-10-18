//! What an A2A task reports when the agent's run stops to wait on its caller, and what
//! of the model's output reaches the peer.

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, Content, Event, EventStream, InvocationContext, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, Result as AdkResult, SingleAgentLoader, Tool, ToolContext,
};
use adk_server::{ServerConfig, create_app_with_a2a};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

const SECRET_REASONING: &str = "SECRET-REASONING: private to the agent";

/// Calls `delete_record` or `start_export` when asked to, reasons before answering when
/// asked to think, and answers `Done.` once a tool result is in the conversation.
struct ScriptedModel;

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        _stream: bool,
    ) -> AdkResult<LlmResponseStream> {
        let last = req.contents.last().cloned().unwrap_or_else(|| Content::new("user"));
        let answered = last.parts.iter().any(|part| matches!(part, Part::FunctionResponse { .. }));
        let text: String = last.parts.iter().filter_map(Part::text).collect();
        let call = |name: &str, args: Value| Part::FunctionCall {
            name: name.to_string(),
            args,
            id: Some(format!("call-{name}")),
            thought_signature: None,
        };
        let parts = if answered {
            vec![Part::Text { text: "Done.".to_string() }]
        } else if text.contains("delete") {
            vec![call("delete_record", json!({"record_id": "R-17"}))]
        } else if text.contains("export") {
            vec![call("start_export", json!({"export_id": "E-9"}))]
        } else if text.contains("think") {
            vec![
                Part::Thinking { thinking: SECRET_REASONING.to_string(), signature: None },
                Part::Text { text: "The answer is 42.".to_string() },
            ]
        } else {
            vec![Part::Text { text: "pong".to_string() }]
        };
        let response = LlmResponse {
            content: Some(Content { role: "model".to_string(), parts }),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

struct RecordTool {
    name: &'static str,
    long_running: bool,
}

#[async_trait]
impl Tool for RecordTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "operates on a record"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({"type": "object", "properties": {}}))
    }

    fn is_long_running(&self) -> bool {
        self.long_running
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
        Ok(json!({"status": "pending", "args": args}))
    }
}

fn ops_agent() -> Arc<dyn Agent> {
    Arc::new(
        LlmAgentBuilder::new("ops_agent")
            .description("deletes records and starts exports")
            .model(Arc::new(ScriptedModel))
            .tool(Arc::new(RecordTool { name: "delete_record", long_running: false }))
            .tool(Arc::new(RecordTool { name: "start_export", long_running: true }))
            .require_tool_confirmation("delete_record")
            .build()
            .unwrap(),
    )
}

/// Asks for credentials the way adk-python agents do, with an `adk_request_credential` call.
struct CredentialAgent;

#[async_trait]
impl Agent for CredentialAgent {
    fn name(&self) -> &str {
        "credential_agent"
    }

    fn description(&self) -> &str {
        "needs a credential"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        let mut event = Event::new(ctx.invocation_id());
        event.author = "credential_agent".to_string();
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "adk_request_credential".to_string(),
                args: json!({"function_call_id": "call-1"}),
                id: Some("call-auth".to_string()),
                thought_signature: None,
            }],
        });
        event.long_running_tool_ids = vec!["adk_request_credential".to_string()];
        Ok(Box::pin(futures::stream::iter([Ok(event)])))
    }
}

fn legacy_app(agent: Arc<dyn Agent>) -> Router {
    let config = ServerConfig::new(
        Arc::new(SingleAgentLoader::new(agent)),
        Arc::new(InMemorySessionService::new()),
    );
    create_app_with_a2a(config, Some("http://localhost:8080"))
}

/// Sends `text` with JSON-RPC `method` and returns the response body.
async fn legacy_call(app: &Router, path: &str, method: &str, text: &str) -> String {
    let body = json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": {"message": {"role": "user", "parts": [{"text": text}], "messageId": text}},
        "id": 1,
    });
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

async fn legacy_state(agent: Arc<dyn Agent>, text: &str) -> String {
    let body = legacy_call(&legacy_app(agent), "/a2a", "message/send", text).await;
    let response: Value = serde_json::from_str(&body).unwrap();
    response["result"]["status"]["state"].as_str().unwrap_or_default().to_string()
}

/// The `state` of every status update in an SSE body, in order.
fn streamed_states(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
        .filter_map(|frame| frame["result"]["status"]["state"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn a_held_tool_call_leaves_the_task_input_required() {
    assert_eq!(legacy_state(ops_agent(), "please delete record R-17").await, "input-required");
}

#[tokio::test]
async fn a_held_tool_call_streams_input_required_as_the_final_status() {
    let body =
        legacy_call(&legacy_app(ops_agent()), "/a2a/stream", "message/stream", "delete R-17").await;
    assert_eq!(streamed_states(&body).last().map(String::as_str), Some("input-required"));
}

#[tokio::test]
async fn a_long_running_call_leaves_the_task_input_required() {
    assert_eq!(legacy_state(ops_agent(), "start export E-9").await, "input-required");
}

#[tokio::test]
async fn a_credential_request_leaves_the_task_auth_required() {
    assert_eq!(legacy_state(Arc::new(CredentialAgent), "read my mail").await, "auth-required");
}

#[tokio::test]
async fn a_run_that_needs_nothing_completes() {
    assert_eq!(legacy_state(ops_agent(), "ping").await, "completed");
}

#[tokio::test]
async fn thinking_never_reaches_the_peer() {
    let app = legacy_app(ops_agent());
    let sent = legacy_call(&app, "/a2a", "message/send", "think: six times seven").await;
    let streamed = legacy_call(&app, "/a2a/stream", "message/stream", "think again").await;
    for body in [sent, streamed] {
        assert!(!body.contains(SECRET_REASONING), "reasoning reached the peer: {body}");
        assert!(body.contains("The answer is 42."), "answer missing: {body}");
    }
}

#[cfg(feature = "a2a-v1")]
mod v1 {
    use super::*;
    use a2a_protocol_types::events::StreamResponse;
    use a2a_protocol_types::task::TaskState;
    use adk_server::a2a::v1::card::{CachedAgentCard, build_v1_agent_card};
    use adk_server::a2a::v1::executor::V1Executor;
    use adk_server::a2a::v1::push::NoOpPushNotificationSender;
    use adk_server::a2a::v1::request_handler::RequestHandler;
    use adk_server::a2a::v1::task_store::InMemoryTaskStore;
    use futures::StreamExt;

    fn handler(agent: Arc<dyn Agent>) -> RequestHandler {
        let store = Arc::new(InMemoryTaskStore::new());
        let card = build_v1_agent_card(
            agent.name(),
            agent.description(),
            "http://localhost:8080/a2a",
            "1.0.0",
            vec![],
            a2a_protocol_types::AgentCapabilities::none(),
        );
        let runner_config = adk_runner::Runner::builder()
            .app_name(agent.name())
            .agent(agent)
            .session_service(Arc::new(InMemorySessionService::new()))
            .build_config();
        RequestHandler::with_runner(
            Arc::new(V1Executor::new(store.clone())),
            store,
            Arc::new(NoOpPushNotificationSender),
            Arc::new(tokio::sync::RwLock::new(CachedAgentCard::new(card))),
            Arc::new(runner_config),
        )
    }

    fn message(text: &str) -> a2a_protocol_types::Message {
        a2a_protocol_types::Message {
            id: a2a_protocol_types::MessageId::new(text),
            role: a2a_protocol_types::MessageRole::User,
            parts: vec![a2a_protocol_types::Part::text(text)],
            task_id: None,
            context_id: None,
            reference_task_ids: None,
            extensions: None,
            metadata: None,
        }
    }

    async fn streamed_final_state(agent: Arc<dyn Agent>, text: &str) -> Option<TaskState> {
        let handler = handler(agent);
        let mut stream = handler.message_stream(message(text)).await.unwrap();
        let mut last = None;
        while let Some(item) = stream.next().await {
            if let Ok(StreamResponse::StatusUpdate(update)) = item {
                last = Some(update.status.state);
            }
        }
        last
    }

    #[tokio::test]
    async fn v1_held_and_long_running_calls_leave_the_task_input_required() {
        let handler = handler(ops_agent());
        let held = handler.message_send(message("please delete record R-17")).await.unwrap();
        assert_eq!(held.status.state, TaskState::InputRequired);
        let long_running = handler.message_send(message("start export E-9")).await.unwrap();
        assert_eq!(long_running.status.state, TaskState::InputRequired);
        let plain = handler.message_send(message("ping")).await.unwrap();
        assert_eq!(plain.status.state, TaskState::Completed);
    }

    #[tokio::test]
    async fn v1_stream_ends_input_required_after_a_held_call() {
        assert_eq!(
            streamed_final_state(ops_agent(), "please delete record R-17").await,
            Some(TaskState::InputRequired)
        );
    }

    #[tokio::test]
    async fn v1_credential_request_leaves_the_task_auth_required() {
        let task = handler(Arc::new(CredentialAgent)).message_send(message("mail")).await.unwrap();
        assert_eq!(task.status.state, TaskState::AuthRequired);
        assert_eq!(
            streamed_final_state(Arc::new(CredentialAgent), "mail again").await,
            Some(TaskState::AuthRequired)
        );
    }
}
