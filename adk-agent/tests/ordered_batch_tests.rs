//! Calls to tools in one ordered batch (`Tool::batch_group`) run in the model's order and stop
//! at the first that does not complete, under every dispatch strategy.

use adk_agent::LlmAgentBuilder;
use adk_core::{
    AdkError, BatchGroup, Content, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result,
    SessionId, Tool, ToolContext, ToolExecutionStrategy, ToolResultStatus, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HALT: &str = "Not executed: an earlier step failed.";

/// A step that sleeps, then fails when its arguments say so, and records that it ran.
struct Step {
    name: &'static str,
    group: Option<&'static str>,
    ran: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Tool for Step {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "one step"
    }

    fn batch_group(&self) -> Option<BatchGroup> {
        self.group.map(|group| BatchGroup::new(group, HALT))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        tokio::time::sleep(Duration::from_millis(args["sleep_ms"].as_u64().unwrap_or(0))).await;
        self.ran.lock().unwrap().push(format!("{}:{}", self.name, args["id"]));
        if args["fail"] == json!(true) {
            return Err(AdkError::tool("the step failed"));
        }
        Ok(json!("done"))
    }
}

struct ScriptedModel {
    responses: Mutex<VecDeque<LlmResponse>>,
    requests: Mutex<Vec<LlmRequest>>,
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        self.requests.lock().unwrap().push(req);
        let response = self.responses.lock().unwrap().pop_front().unwrap_or_else(|| LlmResponse {
            content: Some(Content::new("model").with_text("done")),
            turn_complete: true,
            ..Default::default()
        });
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

fn call(name: &str, args: Value, id: &str) -> Part {
    Part::FunctionCall { name: name.into(), args, id: Some(id.into()), thought_signature: None }
}

/// Runs one model turn with `calls` and returns what ran and how each call was answered.
async fn run(
    strategy: ToolExecutionStrategy,
    calls: Vec<Part>,
) -> (Vec<String>, HashMap<String, (Value, Option<ToolResultStatus>)>) {
    let ran = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(ScriptedModel {
        responses: Mutex::new(
            vec![LlmResponse {
                content: Some(Content { role: "model".into(), parts: calls }),
                turn_complete: true,
                ..Default::default()
            }]
            .into(),
        ),
        requests: Mutex::new(Vec::new()),
    });
    let step = |name, group| Arc::new(Step { name, group, ran: ran.clone() }) as Arc<dyn Tool>;
    let agent = LlmAgentBuilder::new("worker")
        .model(model.clone())
        .tool(step("click", Some("desk")))
        .tool(step("type", Some("desk")))
        .tool(step("note", None))
        .tool_execution_strategy(strategy)
        .build()
        .unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "batch".into(),
            user_id: "user".into(),
            session_id: Some("session".into()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("batch")
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .build()
        .unwrap();
    let stream = runner
        .run(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text("go"),
        )
        .await
        .unwrap();
    let _ =
        tokio::time::timeout(Duration::from_secs(10), stream.collect::<Vec<_>>()).await.unwrap();
    let requests = model.requests.lock().unwrap();
    let answers = requests
        .last()
        .unwrap()
        .contents
        .iter()
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, id, .. } => Some((
                id.clone().unwrap_or_default(),
                (function_response.response.clone(), part.tool_result_status()),
            )),
            _ => None,
        })
        .collect();
    let ran = ran.lock().unwrap().clone();
    (ran, answers)
}

#[tokio::test]
async fn a_failed_call_halts_the_rest_of_its_batch_only() {
    for strategy in [
        ToolExecutionStrategy::Sequential,
        ToolExecutionStrategy::Parallel,
        ToolExecutionStrategy::Auto,
    ] {
        let (ran, answers) = run(
            strategy,
            vec![
                // The slow failing click would finish last if the batch overlapped.
                call("click", json!({ "id": 1, "sleep_ms": 50, "fail": true }), "c1"),
                call("type", json!({ "id": 2 }), "c2"),
                call("note", json!({ "id": 3 }), "c3"),
                call("type", json!({ "id": 4 }), "c4"),
            ],
        )
        .await;
        assert_eq!(ran, vec!["click:1".to_string(), "note:3".to_string()], "{strategy:?}");
        assert_eq!(answers["c1"].1, Some(ToolResultStatus::Failed), "{strategy:?}");
        for id in ["c2", "c4"] {
            assert_eq!(answers[id], (json!(HALT), Some(ToolResultStatus::NotRun)), "{strategy:?}");
        }
        // A tool outside the batch still runs.
        assert_eq!(answers["c3"].0, json!("done"), "{strategy:?}");
    }
}

#[tokio::test]
async fn a_batch_that_succeeds_runs_every_call_in_order() {
    let (ran, answers) = run(
        ToolExecutionStrategy::Parallel,
        vec![
            call("click", json!({ "id": 1, "sleep_ms": 40 }), "c1"),
            call("type", json!({ "id": 2 }), "c2"),
        ],
    )
    .await;
    assert_eq!(ran, vec!["click:1".to_string(), "type:2".to_string()]);
    assert!(answers.values().all(|answer| answer.1.is_none()), "{answers:?}");
}
