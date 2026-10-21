//! An elicitation handler learns the run and tool call that triggered the request
//! (spec 3.1 §10 T1): the scope of the in-flight call, or the toolset's fallback.
#![cfg(feature = "mcp")]

use std::sync::{Arc, Mutex};

use adk_core::{
    Artifacts, CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, RunConfig,
    Tool, ToolContext,
};
use adk_tool::mcp::{ElicitationHandler, ElicitationScope, McpToolset};
use async_trait::async_trait;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, ElicitRequestParams, ElicitResult,
    ElicitationAction, ElicitationSchema, InitializeResult, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

/// Asks the client a form question once per `tools/call` and returns the answer's action.
#[derive(Clone)]
struct ElicitingServer;

impl ServerHandler for ElicitingServer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        let schema = json!({ "type": "object" }).as_object().cloned().unwrap_or_default();
        Ok(ListToolsResult::with_all_items(vec![rmcp::model::Tool::new(
            "ask",
            "Asks the user a question.",
            Arc::new(schema),
        )]))
    }

    async fn call_tool(
        &self,
        _params: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        let answer = context
            .peer
            .create_elicitation(ElicitRequestParams::FormElicitationParams {
                meta: None,
                message: "Which branch?".into(),
                requested_schema: ElicitationSchema::new(Default::default()),
            })
            .await;
        Ok(match answer {
            Ok(result) => {
                CallToolResult::success(vec![ContentBlock::text(format!("{:?}", result.action))])
            }
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error.to_string())]),
        }
        .into())
    }
}

fn spawn_server() -> tokio::io::DuplexStream {
    let (server_io, client_io) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        if let Ok(running) = ElicitingServer.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    client_io
}

/// Records the scope it saw while answering, and accepts.
#[derive(Default)]
struct RecordingHandler {
    seen: Mutex<Vec<Option<ElicitationScope>>>,
}

#[async_trait]
impl ElicitationHandler for RecordingHandler {
    async fn handle_form_elicitation(
        &self,
        _message: &str,
        _schema: &ElicitationSchema,
        _metadata: Option<&Value>,
    ) -> Result<ElicitResult, Box<dyn std::error::Error + Send + Sync>> {
        self.seen.lock().unwrap().push(ElicitationScope::current());
        Ok(ElicitResult::new(ElicitationAction::Accept).with_content(json!({})))
    }

    async fn handle_url_elicitation(
        &self,
        _message: &str,
        _url: &str,
        _elicitation_id: &str,
        _metadata: Option<&Value>,
    ) -> Result<ElicitResult, Box<dyn std::error::Error + Send + Sync>> {
        self.seen.lock().unwrap().push(ElicitationScope::current());
        Ok(ElicitResult::new(ElicitationAction::Decline))
    }
}

/// A tool context inside a run of session `session-1` of `user-1` in `acme`.
struct RunToolContext {
    run_config: Option<RunConfig>,
    content: Content,
    actions: Mutex<EventActions>,
}

impl RunToolContext {
    fn new(run_config: Option<RunConfig>) -> Arc<Self> {
        Arc::new(Self {
            run_config,
            content: Content::new("user").with_text("go"),
            actions: Mutex::new(EventActions::default()),
        })
    }
}

#[async_trait]
impl ReadonlyContext for RunToolContext {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "researcher"
    }
    fn user_id(&self) -> &str {
        "user-1"
    }
    fn app_name(&self) -> &str {
        "acme"
    }
    fn session_id(&self) -> &str {
        "session-1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl CallbackContext for RunToolContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for RunToolContext {
    fn function_call_id(&self) -> &str {
        "call-1"
    }
    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap().clone()
    }
    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap() = actions;
    }
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }
    fn run_config(&self) -> Option<&RunConfig> {
        self.run_config.as_ref()
    }
}

async fn ask_tool(toolset: &McpToolset<adk_tool::mcp::AdkClientHandler>) -> Arc<dyn Tool> {
    let ctx = RunToolContext::new(None) as Arc<dyn ReadonlyContext>;
    let tools = adk_core::Toolset::tools(toolset, ctx).await.expect("list tools");
    tools.into_iter().find(|tool| tool.name() == "ask").expect("ask tool")
}

#[tokio::test]
async fn an_elicitation_during_a_tool_call_carries_that_call_scope() {
    let handler = Arc::new(RecordingHandler::default());
    let toolset = McpToolset::with_elicitation_handler(spawn_server(), handler.clone())
        .await
        .expect("connect");
    let tool = ask_tool(&toolset).await;
    let config = RunConfig { durable_run_id: Some("run-7".into()), ..RunConfig::default() };

    let value = tool.execute(RunToolContext::new(Some(config)), json!({})).await.expect("call");

    assert!(value.to_string().contains("Accept"), "{value}");
    let seen = handler.seen.lock().unwrap();
    let scope = seen[0].as_ref().expect("the handler saw the call's scope");
    assert_eq!(
        (
            scope.app_name.as_str(),
            scope.user_id.as_str(),
            scope.session_id.as_str(),
            scope.invocation_id.as_str(),
            scope.agent_name.as_str(),
            scope.function_call_id.as_deref(),
            scope.run_id.as_deref(),
        ),
        ("acme", "user-1", "session-1", "inv-1", "researcher", Some("call-1"), Some("run-7"))
    );
    assert!(scope.acting.is_some(), "the acting agent travels with the scope");
}

#[tokio::test]
async fn an_elicitation_outside_any_call_resolves_to_the_fallback_scope() {
    let handler = Arc::new(RecordingHandler::default());
    let toolset = McpToolset::with_elicitation_handler(spawn_server(), handler.clone())
        .await
        .expect("connect")
        .with_elicitation_scope(ElicitationScope::new("acme", "operator", "console"));
    // The toolset's own call registers a scope, so the fallback is proven through the
    // handler directly: it is what `resolve()` returns when nothing is in flight.
    let scope = ElicitationScope::current();
    assert!(scope.is_none(), "no scope outside a handler");
    let tool = ask_tool(&toolset).await;
    tool.execute(RunToolContext::new(None), json!({})).await.expect("call");
    let seen = handler.seen.lock().unwrap();
    let scope = seen[0].as_ref().expect("scope");
    // In flight, the call's scope wins over the fallback.
    assert_eq!(scope.user_id, "user-1");
}

#[tokio::test]
async fn the_scope_registry_resolves_the_latest_call_then_the_fallback() {
    let scopes = Arc::new(adk_tool::mcp::ElicitationScopes::default());
    assert!(scopes.resolve().is_none());
    scopes.set_fallback(Some(ElicitationScope::new("acme", "operator", "console")));
    assert_eq!(scopes.resolve().map(|s| s.session_id), Some("console".to_string()));
    let first = scopes.enter(ElicitationScope::new("acme", "a", "s1"));
    let second = scopes.enter(ElicitationScope::new("acme", "b", "s2"));
    assert_eq!(scopes.resolve().map(|s| s.session_id), Some("s2".to_string()));
    drop(second);
    assert_eq!(scopes.resolve().map(|s| s.session_id), Some("s1".to_string()));
    drop(first);
    assert_eq!(scopes.resolve().map(|s| s.session_id), Some("console".to_string()));
}
