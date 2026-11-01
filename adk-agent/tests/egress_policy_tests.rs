//! The operator's egress ceiling on the governed path (#1281): a `Runner` with an egress
//! policy refuses tool calls whose `Tool::egress` declaration it does not admit, before they
//! run, and a run's own policy narrows the ceiling without widening it.
//!
//! Every test drives a real `LlmAgent` through a `Runner` with `adk_model::ScriptedLlm`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::egress::{EgressPolicy, ToolEgress};
use adk_core::{
    Content, Event, Llm, Part, Result, RunConfig, SessionId, Tool, ToolContext, UserId,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

/// Records each execution and declares the URL argument as its destination, or nothing.
struct Fetch {
    name: &'static str,
    declares: bool,
    executions: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl Tool for Fetch {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "fetches a URL"
    }

    fn egress(&self, args: &Value) -> ToolEgress {
        if self.declares {
            ToolEgress::for_urls([args["url"].as_str().unwrap_or_default()])
        } else {
            ToolEgress::Undeclared
        }
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let policy = ctx.egress_policy().map(ToString::to_string);
        self.executions.lock().unwrap().push(json!({ "args": args, "policy": policy }));
        Ok(json!({ "status": "ok" }))
    }
}

fn agent(model: Arc<ScriptedLlm>, tools: Vec<Arc<dyn Tool>>) -> LlmAgent {
    let mut builder = LlmAgentBuilder::new("fetcher")
        .model(model as Arc<dyn Llm>)
        .tool_execution_strategy(adk_core::ToolExecutionStrategy::Sequential);
    for tool in tools {
        builder = builder.tool(tool);
    }
    builder.build().unwrap()
}

async fn runner(agent: LlmAgent, ceiling: Option<EgressPolicy>) -> Runner {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "app".to_string(),
            user_id: "alice".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let builder = Runner::builder()
        .app_name("app")
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>);
    match ceiling {
        Some(ceiling) => builder.egress_policy(ceiling).build().unwrap(),
        None => builder.build().unwrap(),
    }
}

async fn run(runner: &Runner, config: Option<RunConfig>) -> Vec<Event> {
    let stream = runner
        .run_with_config(
            UserId::new("alice").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text("go"),
            config,
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), stream.collect::<Vec<_>>())
        .await
        .expect("the run must finish")
        .into_iter()
        .map(|event| event.expect("the run must not fail"))
        .collect()
}

/// The function responses the model was given, in order.
fn told(model: &ScriptedLlm) -> Vec<Value> {
    model
        .last_request()
        .expect("the model was called")
        .contents
        .iter()
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .collect()
}

fn fetch(name: &'static str, declares: bool) -> (Arc<dyn Tool>, Arc<Mutex<Vec<Value>>>) {
    let executions = Arc::new(Mutex::new(Vec::new()));
    (Arc::new(Fetch { name, declares, executions: executions.clone() }), executions)
}

#[tokio::test]
async fn the_runner_ceiling_refuses_undeclared_and_uncovered_calls_before_they_run() {
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        [
            ScriptedResponse::function_calls([
                ("declared", json!({ "url": "https://api.github.com/zen" })),
                ("declared", json!({ "url": "https://evil.test/upload" })),
                ("undeclared", json!({ "url": "https://api.github.com/zen" })),
            ]),
            ScriptedResponse::text("done"),
        ],
    ));
    let (declared, declared_runs) = fetch("declared", true);
    let (undeclared, undeclared_runs) = fetch("undeclared", false);
    let ceiling = EgressPolicy::allowlist(["api.github.com:443"]).unwrap();
    let runner = runner(agent(model.clone(), vec![declared, undeclared]), Some(ceiling)).await;

    run(&runner, None).await;

    let runs = declared_runs.lock().unwrap().clone();
    assert_eq!(runs.len(), 1, "only the covered call runs: {runs:?}");
    assert_eq!(runs[0]["args"], json!({ "url": "https://api.github.com/zen" }));
    assert_eq!(runs[0]["policy"], json!("api.github.com:443"), "the tool sees the ceiling");
    assert!(undeclared_runs.lock().unwrap().is_empty(), "an undeclared call never runs");
    let responses = told(&model);
    assert_eq!(responses[0], json!({ "status": "ok" }));
    for refused in &responses[1..] {
        let text = refused.to_string();
        assert!(text.contains("egress policy"), "the model is told why: {text}");
    }
}

#[tokio::test]
async fn a_run_policy_narrows_the_ceiling_and_cannot_widen_it() {
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        [
            ScriptedResponse::function_calls([
                ("declared", json!({ "url": "https://api.github.com/zen" })),
                ("declared", json!({ "url": "https://docs.rs/adk-core" })),
                ("declared", json!({ "url": "https://evil.test/" })),
            ]),
            ScriptedResponse::text("done"),
        ],
    ));
    let (declared, runs) = fetch("declared", true);
    let ceiling = EgressPolicy::allowlist(["api.github.com:443", "docs.rs:443"]).unwrap();
    let runner = runner(agent(model.clone(), vec![declared]), Some(ceiling)).await;
    // The run asks for docs.rs and evil.test: docs.rs is outside it, evil.test outside the ceiling.
    let config = RunConfig::builder()
        .egress_policy(EgressPolicy::allowlist(["api.github.com:443", "evil.test:443"]).unwrap())
        .build();

    run(&runner, Some(config)).await;

    let urls: Vec<Value> =
        runs.lock().unwrap().iter().map(|run| run["args"]["url"].clone()).collect();
    assert_eq!(urls, vec![json!("https://api.github.com/zen")]);
}

#[tokio::test]
async fn without_an_egress_policy_no_declaration_is_consulted() {
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        [
            ScriptedResponse::function_calls([(
                "undeclared",
                json!({ "url": "https://evil.test/" }),
            )]),
            ScriptedResponse::text("done"),
        ],
    ));
    let (undeclared, runs) = fetch("undeclared", false);
    let runner = runner(agent(model, vec![undeclared]), None).await;

    run(&runner, None).await;

    assert_eq!(runs.lock().unwrap().len(), 1);
    assert_eq!(runs.lock().unwrap()[0]["policy"], Value::Null);
}
