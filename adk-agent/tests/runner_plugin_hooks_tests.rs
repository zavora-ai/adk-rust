//! A runner's plugin model, tool, and agent callbacks run inside `LlmAgent`.
//!
//! `Runner` called only the run, user-message, and event callbacks of its `PluginManager`; the
//! model, tool, and agent callbacks had no caller, so a plugin that denied a tool or replaced a
//! model call silently did nothing.
//!
//! Content a tool callback substitutes for a result answers the call it replaces, so providers
//! that pair every function call with a response by id accept the next request.

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{
    BeforeModelResult, CallbackContext, Content, Event, FunctionResponseData, Llm, LlmRequest,
    LlmResponse, LlmResponseStream, Part, Result, SessionId, Tool, ToolContext, UserId,
};
use adk_plugin::{Plugin, PluginConfig, PluginManager};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Replays scripted responses, counts how often it is called, and records each request.
struct ScriptedModel {
    responses: Mutex<VecDeque<LlmResponse>>,
    calls: AtomicUsize,
    requests: Mutex<Vec<LlmRequest>>,
}

impl ScriptedModel {
    fn new(responses: Vec<LlmResponse>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into()),
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(req);
        let response =
            self.responses.lock().unwrap().pop_front().unwrap_or_else(|| text_response("done"));
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

fn text_response(text: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content::new("model").with_text(text)),
        turn_complete: true,
        ..Default::default()
    }
}

fn call_response(tool: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: tool.to_string(),
                args: json!({ "path": "/srv/data" }),
                id: Some("call-1".to_string()),
                thought_signature: None,
            }],
        }),
        turn_complete: true,
        ..Default::default()
    }
}

/// Counts executions; fails when `fail` is set.
struct DeleteTool {
    calls: Arc<AtomicUsize>,
    fail: bool,
}

#[async_trait]
impl Tool for DeleteTool {
    fn name(&self) -> &str {
        "delete_file"
    }

    fn description(&self) -> &str {
        "Deletes a file"
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(adk_core::AdkError::tool("disk unavailable"));
        }
        Ok(json!({ "deleted": true }))
    }
}

async fn run(model: Arc<ScriptedModel>, tool: DeleteTool, plugin: Plugin) -> Vec<Event> {
    run_agent(
        LlmAgentBuilder::new("ops")
            .model(model as Arc<dyn Llm>)
            .tool(Arc::new(tool))
            .build()
            .unwrap(),
        plugin,
    )
    .await
}

async fn run_agent(agent: LlmAgent, plugin: Plugin) -> Vec<Event> {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "plugins".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("plugins")
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .plugin_manager(Arc::new(PluginManager::new(vec![plugin])))
        .build()
        .unwrap();
    let stream = runner
        .run(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text("delete it"),
        )
        .await
        .unwrap();
    stream.map(|event| event.unwrap()).collect().await
}

fn function_responses(events: &[Event]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .collect()
}

/// The function call ids of `request`, and the ids its function responses answer, both sorted.
///
/// OpenAI and Anthropic reject a request whose function call has no response with its id.
fn call_and_response_ids(request: &LlmRequest) -> (Vec<Option<String>>, Vec<Option<String>>) {
    let mut calls = Vec::new();
    let mut responses = Vec::new();
    for part in request.contents.iter().flat_map(|content| content.parts.iter()) {
        match part {
            Part::FunctionCall { id, .. } => calls.push(id.clone()),
            Part::FunctionResponse { id, .. } => responses.push(id.clone()),
            _ => {}
        }
    }
    calls.sort();
    responses.sort();
    (calls, responses)
}

/// Asserts the model's second request answers its one function call exactly once.
fn assert_call_answered(model: &ScriptedModel) {
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "the model is called again after the tool call");
    let one_call = vec![Some("call-1".to_string())];
    assert_eq!(call_and_response_ids(&requests[1]), (one_call.clone(), one_call));
}

/// A plugin that runs `before_tool`, or `after_tool` when `after` is set, returning `content`.
fn substituting_plugin(content: Content, after: bool) -> Plugin {
    let callback: adk_core::BeforeToolCallback = Box::new(move |_ctx| {
        let content = content.clone();
        Box::pin(async move { Ok(Some(content)) })
    });
    let (before_tool, after_tool) =
        if after { (None, Some(callback)) } else { (Some(callback), None) };
    Plugin::new(PluginConfig {
        name: "substitute".to_string(),
        before_tool,
        after_tool,
        ..Default::default()
    })
}

fn texts(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(Part::text)
        .collect::<Vec<_>>()
        .join("|")
}

#[tokio::test]
async fn a_runner_plugin_before_tool_callback_blocks_the_tool() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_in_plugin = Arc::clone(&seen);
    let plugin = Plugin::new(PluginConfig {
        name: "deny-deletes".to_string(),
        before_tool: Some(Box::new(move |ctx: Arc<dyn CallbackContext>| {
            let seen = Arc::clone(&seen_in_plugin);
            Box::pin(async move {
                let name = ctx.tool_name().unwrap_or_default().to_string();
                seen.lock().unwrap().push((name.clone(), ctx.tool_input().cloned()));
                if name == "delete_file" {
                    return Ok(Some(Content::new("function").with_text("blocked by policy")));
                }
                Ok(None)
            })
        })),
        ..Default::default()
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let model = ScriptedModel::new(vec![call_response("delete_file"), text_response("ok")]);

    let events =
        run(Arc::clone(&model), DeleteTool { calls: Arc::clone(&calls), fail: false }, plugin)
            .await;

    assert_eq!(calls.load(Ordering::SeqCst), 0, "the plugin's denial must stop the tool");
    assert_eq!(function_responses(&events), vec![json!({ "error": "blocked by policy" })]);
    assert_call_answered(&model);
    assert_eq!(
        *seen.lock().unwrap(),
        vec![("delete_file".to_string(), Some(json!({ "path": "/srv/data" })))]
    );
}

#[tokio::test]
async fn a_runner_plugin_before_model_callback_can_skip_the_model() {
    let plugin = Plugin::new(PluginConfig {
        name: "canned".to_string(),
        before_model: Some(Box::new(|_ctx, _request| {
            Box::pin(async { Ok(BeforeModelResult::Skip(text_response("served by plugin"))) })
        })),
        ..Default::default()
    });
    let model = ScriptedModel::new(vec![text_response("served by model")]);

    let events = run(
        Arc::clone(&model),
        DeleteTool { calls: Arc::new(AtomicUsize::new(0)), fail: false },
        plugin,
    )
    .await;

    assert_eq!(model.calls.load(Ordering::SeqCst), 0, "the model must not be called");
    assert_eq!(texts(&events), "served by plugin");
}

#[tokio::test]
async fn runner_plugin_agent_model_and_tool_callbacks_all_run() {
    let counts: Arc<Mutex<HashMap<&'static str, usize>>> = Arc::default();
    let count = |name: &'static str| {
        let counts = Arc::clone(&counts);
        move || *counts.lock().unwrap().entry(name).or_default() += 1
    };
    let (before_agent, after_agent, after_model, after_tool) =
        (count("before_agent"), count("after_agent"), count("after_model"), count("after_tool"));
    let on_tool_error = count("on_tool_error");
    let plugin = Plugin::new(PluginConfig {
        name: "observer".to_string(),
        before_agent: Some(Box::new(move |_ctx| {
            before_agent();
            Box::pin(async { Ok(None) })
        })),
        after_agent: Some(Box::new(move |_ctx| {
            after_agent();
            Box::pin(async { Ok(None) })
        })),
        after_model: Some(Box::new(move |_ctx, _response| {
            after_model();
            Box::pin(async { Ok(None) })
        })),
        on_tool_error: Some(Box::new(move |_ctx, _tool, _args, _error| {
            on_tool_error();
            Box::pin(async { Ok(Some(json!({ "fallback": "from plugin" }))) })
        })),
        after_tool: Some(Box::new(move |_ctx| {
            after_tool();
            Box::pin(async { Ok(None) })
        })),
        ..Default::default()
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let model = ScriptedModel::new(vec![call_response("delete_file"), text_response("ok")]);

    let events =
        run(Arc::clone(&model), DeleteTool { calls: Arc::clone(&calls), fail: true }, plugin).await;

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(function_responses(&events), vec![json!({ "fallback": "from plugin" })]);
    assert_eq!(
        *counts.lock().unwrap(),
        HashMap::from([
            ("before_agent", 1),
            ("after_agent", 1),
            ("after_model", 2),
            ("on_tool_error", 1),
            ("after_tool", 1),
        ])
    );
}

#[tokio::test]
async fn runner_plugin_callbacks_reach_an_agent_behind_an_agent_tool() {
    let models = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&models);
    let plugin = Plugin::new(PluginConfig {
        name: "model-audit".to_string(),
        before_model: Some(Box::new(move |ctx, request| {
            seen.lock().unwrap().push(ctx.agent_name().to_string());
            Box::pin(async move { Ok(BeforeModelResult::Continue(request)) })
        })),
        ..Default::default()
    });
    let child = LlmAgentBuilder::new("researcher")
        .description("Looks things up")
        .model(ScriptedModel::new(vec![text_response("found it")]) as Arc<dyn Llm>)
        .build()
        .unwrap();
    let parent = LlmAgentBuilder::new("lead")
        .model(ScriptedModel::new(vec![call_response("researcher"), text_response("done")])
            as Arc<dyn Llm>)
        .tool(Arc::new(adk_tool::AgentTool::new(Arc::new(child))))
        .build()
        .unwrap();

    run_agent(parent, plugin).await;

    assert_eq!(*models.lock().unwrap(), vec!["lead", "researcher", "lead"]);
}

#[tokio::test]
async fn an_agent_before_tool_denial_answers_the_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let model = ScriptedModel::new(vec![call_response("delete_file"), text_response("ok")]);
    let agent = LlmAgentBuilder::new("ops")
        .model(Arc::clone(&model) as Arc<dyn Llm>)
        .tool(Arc::new(DeleteTool { calls: Arc::clone(&calls), fail: false }))
        .before_tool_callback(Box::new(|_ctx| {
            Box::pin(async { Ok(Some(Content::new("tool").with_text("denied by policy"))) })
        }))
        .build()
        .unwrap();

    let events = run_agent(
        agent,
        Plugin::new(PluginConfig { name: "none".to_string(), ..Default::default() }),
    )
    .await;

    assert_eq!(calls.load(Ordering::SeqCst), 0, "the denial must stop the tool");
    assert_eq!(function_responses(&events), vec![json!({ "error": "denied by policy" })]);
    assert_call_answered(&model);
}

#[tokio::test]
async fn an_after_tool_text_override_answers_the_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let model = ScriptedModel::new(vec![call_response("delete_file"), text_response("ok")]);

    let events = run(
        Arc::clone(&model),
        DeleteTool { calls: Arc::clone(&calls), fail: false },
        substituting_plugin(Content::new("function").with_text("result redacted"), true),
    )
    .await;

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(function_responses(&events), vec![json!({ "result": "result redacted" })]);
    assert_call_answered(&model);
}

#[tokio::test]
async fn a_substituted_function_response_is_readdressed_to_the_call() {
    let model = ScriptedModel::new(vec![call_response("delete_file"), text_response("ok")]);
    let substitute = Content {
        role: "function".to_string(),
        parts: vec![
            Part::Text { text: "served from cache".to_string() },
            Part::FunctionResponse {
                function_response: FunctionResponseData::new("cache", json!({ "cached": true })),
                id: Some("stale-id".to_string()),
                annotations: None,
            },
        ],
    };

    let events = run(
        Arc::clone(&model),
        DeleteTool { calls: Arc::new(AtomicUsize::new(0)), fail: false },
        substituting_plugin(substitute, false),
    )
    .await;

    let parts: Vec<Part> = events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .filter(|content| content.role == "function")
        .flat_map(|content| content.parts.clone())
        .collect();
    assert_eq!(
        parts,
        vec![Part::FunctionResponse {
            function_response: FunctionResponseData::new("delete_file", json!({ "cached": true })),
            id: Some("call-1".to_string()),
            annotations: None,
        }]
    );
    assert_call_answered(&model);
}
