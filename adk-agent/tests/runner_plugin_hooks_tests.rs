//! A runner's plugin model, tool, and agent callbacks run inside `LlmAgent`.
//!
//! `Runner` called only the run, user-message, and event callbacks of its `PluginManager`; the
//! model, tool, and agent callbacks had no caller, so a plugin that denied a tool or replaced a
//! model call silently did nothing.

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{
    BeforeModelResult, CallbackContext, Content, Event, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, Result, SessionId, Tool, ToolContext, UserId,
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

/// Replays scripted responses and counts how often it is called.
struct ScriptedModel {
    responses: Mutex<VecDeque<LlmResponse>>,
    calls: AtomicUsize,
}

impl ScriptedModel {
    fn new(responses: Vec<LlmResponse>) -> Arc<Self> {
        Arc::new(Self { responses: Mutex::new(responses.into()), calls: AtomicUsize::new(0) })
    }
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
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
    assert!(texts(&events).contains("blocked by policy"), "events: {events:?}");
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
