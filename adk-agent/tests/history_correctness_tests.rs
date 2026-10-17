//! What each agent's model receives, and what the session keeps: workflow sub-agents see
//! earlier agents' turns as context, `temp:` state lasts for the invocation, streamed turns
//! persist once and whole, empty turns fail, fenced JSON validates, and forwarded `AgentTool`
//! events stay out of the caller's history.

use adk_agent::{LlmAgentBuilder, LoopAgent, SequentialAgent};
use adk_core::history::{OTHER_AGENT_CONTEXT_PREAMBLE, is_other_agent_context};
use adk_core::{
    AdkError, Agent, Content, Event, FinishReason, LlmRequest, LlmResponse, Part, SessionId, Tool,
    ToolContext, UsageMetadata, UserId,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use adk_tool::{AgentTool, FunctionTool};
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const APP: &str = "history";
const USER: &str = "user";
const SESSION: &str = "session";

struct Harness {
    sessions: Arc<InMemorySessionService>,
}

impl Harness {
    async fn new() -> Self {
        let sessions = Arc::new(InMemorySessionService::new());
        sessions
            .create(CreateRequest {
                app_name: APP.to_string(),
                user_id: USER.to_string(),
                session_id: Some(SESSION.to_string()),
                state: HashMap::new(),
            })
            .await
            .unwrap();
        Self { sessions }
    }

    /// Runs `agent` on `prompt`; returns the yielded events and the error that ended the run.
    async fn run(&self, agent: Arc<dyn Agent>, prompt: &str) -> (Vec<Event>, Option<AdkError>) {
        let runner = Runner::builder()
            .app_name(APP)
            .agent(agent)
            .session_service(self.sessions.clone() as Arc<dyn SessionService>)
            .build()
            .unwrap();
        let mut stream = runner
            .run(
                UserId::new(USER).unwrap(),
                SessionId::new(SESSION).unwrap(),
                Content::new("user").with_text(prompt),
            )
            .await
            .unwrap();
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(event) => events.push(event),
                Err(error) => return (events, Some(error)),
            }
        }
        (events, None)
    }

    async fn persisted(&self) -> Vec<Event> {
        self.session().await.events().all()
    }

    async fn state(&self) -> HashMap<String, Value> {
        self.session().await.state().all()
    }

    async fn session(&self) -> Box<dyn adk_session::Session> {
        self.sessions
            .get(GetRequest {
                app_name: APP.to_string(),
                user_id: USER.to_string(),
                session_id: SESSION.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap()
    }
}

/// `(role, text)` per content, with an other-agent context message shown as `context:` and
/// calls and responses as `call:` and `response:`.
fn summarise(request: &LlmRequest) -> Vec<(String, String)> {
    request
        .contents
        .iter()
        .filter(|content| content.role != "system")
        .map(|content| {
            let text = if is_other_agent_context(content) {
                let quoted: Vec<&str> = content.parts[1..].iter().filter_map(Part::text).collect();
                format!("context:{}", quoted.join("|"))
            } else {
                content
                    .parts
                    .iter()
                    .map(|part| match part {
                        Part::Text { text } => text.clone(),
                        Part::FunctionCall { name, .. } => format!("call:{name}"),
                        Part::FunctionResponse { function_response, .. } => {
                            format!("response:{}", function_response.name)
                        }
                        other => format!("{other:?}"),
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            };
            (content.role.clone(), text)
        })
        .collect()
}

fn pair(role: &str, text: &str) -> (String, String) {
    (role.to_string(), text.to_string())
}

fn quoted(text: &str) -> String {
    format!(
        "{}\n{text}\n{}",
        adk_core::history::QUOTED_CONTENT_BEGIN,
        adk_core::history::QUOTED_CONTENT_END
    )
}

fn lookup() -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new("lookup", "Looks up an order.", |_ctx, _args| async move {
        Ok(json!({"status": "ships Monday"}))
    }))
}

fn partial(text: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content::new("model").with_text(text)),
        partial: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_sequential_sub_agent_sees_the_earlier_agents_turns_as_context() {
    let writer_model = Arc::new(ScriptedLlm::new(
        "writer-model",
        [
            ScriptedResponse::function_call("lookup", json!({"order": "A1"})),
            ScriptedResponse::text("A1 ships Monday"),
        ],
    ));
    let reviewer_model =
        Arc::new(ScriptedLlm::new("reviewer-model", [ScriptedResponse::text("approved")]));
    let writer = LlmAgentBuilder::new("writer").model(writer_model).tool(lookup()).build().unwrap();
    let reviewer = LlmAgentBuilder::new("reviewer").model(reviewer_model.clone()).build().unwrap();
    let pipeline = SequentialAgent::new("pipeline", vec![Arc::new(writer), Arc::new(reviewer)]);

    let harness = Harness::new().await;
    let (_, error) = harness.run(Arc::new(pipeline), "where is A1?").await;
    assert!(error.is_none(), "{error:?}");

    let request = reviewer_model.last_request().unwrap();
    assert_eq!(
        summarise(&request),
        vec![
            pair("user", "where is A1?"),
            pair(
                "user",
                &format!(
                    "context:[writer] called tool `lookup` with parameters:\n{}",
                    quoted(r#"{"order":"A1"}"#)
                )
            ),
            pair(
                "user",
                &format!(
                    "context:[writer] `lookup` tool returned result:\n{}",
                    quoted(r#"{"status":"ships Monday"}"#)
                )
            ),
            pair("user", &format!("context:[writer] said:\n{}", quoted("A1 ships Monday"))),
        ]
    );
    assert_eq!(request.contents[1].parts[0].text(), Some(OTHER_AGENT_CONTEXT_PREAMBLE));
}

#[tokio::test]
async fn a_loop_sub_agent_keeps_its_own_turns_and_ends_on_a_user_turn() {
    let model = Arc::new(ScriptedLlm::new(
        "model",
        [
            ScriptedResponse::text("draft 1"),
            ScriptedResponse::text("review 1"),
            ScriptedResponse::text("draft 2"),
            ScriptedResponse::text("review 2"),
        ],
    ));
    let drafter = LlmAgentBuilder::new("drafter").model(model.clone()).build().unwrap();
    let reviewer = LlmAgentBuilder::new("reviewer").model(model.clone()).build().unwrap();
    let refine = LoopAgent::new("refine", vec![Arc::new(drafter), Arc::new(reviewer)])
        .with_max_iterations(2);

    let harness = Harness::new().await;
    let (_, error) = harness.run(Arc::new(refine), "write it").await;
    assert!(error.is_none(), "{error:?}");

    let requests = model.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        summarise(&requests[2]),
        vec![
            pair("user", "write it"),
            pair("model", "draft 1"),
            pair("user", &format!("context:[reviewer] said:\n{}", quoted("review 1"))),
        ],
        "the drafter's second round keeps its own draft and reads the review as context"
    );
    assert_eq!(
        summarise(&requests[3]),
        vec![
            pair("user", "write it"),
            pair("user", &format!("context:[drafter] said:\n{}", quoted("draft 1"))),
            pair("model", "review 1"),
            pair("user", &format!("context:[drafter] said:\n{}", quoted("draft 2"))),
        ]
    );
}

#[tokio::test]
async fn a_single_agent_loop_round_ends_on_a_user_turn() {
    let model = Arc::new(ScriptedLlm::new(
        "model",
        [ScriptedResponse::text("one"), ScriptedResponse::text("two")],
    ));
    let writer = LlmAgentBuilder::new("writer").model(model.clone()).build().unwrap();
    let refine = LoopAgent::new("refine", vec![Arc::new(writer)]).with_max_iterations(2);

    let harness = Harness::new().await;
    harness.run(Arc::new(refine), "go").await;

    let second = summarise(&model.requests()[1]);
    assert_eq!(second[..2], [pair("user", "go"), pair("model", "one")]);
    assert_eq!(second.len(), 3);
    assert_eq!(second[2].0, "user", "a round that follows the agent's own answer gets a user turn");
}

#[tokio::test]
async fn temp_state_is_visible_to_later_tools_and_agents_in_the_invocation() {
    let seen = Arc::new(Mutex::new(None::<Value>));
    let login: Arc<dyn Tool> = Arc::new(FunctionTool::new(
        "login",
        "Signs in.",
        |ctx: Arc<dyn ToolContext>, _args| async move {
            let mut actions = ctx.actions();
            actions.state_delta.insert("temp:token".into(), json!("tok-1"));
            ctx.set_actions(actions);
            Ok(json!({"ok": true}))
        },
    ));
    let observed = seen.clone();
    let use_token: Arc<dyn Tool> = Arc::new(FunctionTool::new(
        "use_token",
        "Uses the token.",
        move |ctx: Arc<dyn ToolContext>, _args| {
            let observed = observed.clone();
            async move {
                let token = ctx.session().and_then(|session| session.state().get("temp:token"));
                *observed.lock().unwrap() = token;
                Ok(json!({}))
            }
        },
    ));
    let front_model = Arc::new(ScriptedLlm::new(
        "front-model",
        [
            ScriptedResponse::function_call("login", json!({})),
            ScriptedResponse::function_call("use_token", json!({})),
            ScriptedResponse::function_call("transfer_to_agent", json!({"agent_name": "back"})),
        ],
    ));
    let back_model = Arc::new(ScriptedLlm::new("back-model", [ScriptedResponse::text("done")]));
    let back = LlmAgentBuilder::new("back")
        .model(back_model.clone())
        .instruction("Token: {temp:token}")
        .build()
        .unwrap();
    let front = LlmAgentBuilder::new("front")
        .model(front_model)
        .tool(login)
        .tool(use_token)
        .sub_agent(Arc::new(back))
        .build()
        .unwrap();

    let harness = Harness::new().await;
    let (_, error) = harness.run(Arc::new(front), "sign in").await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(*seen.lock().unwrap(), Some(json!("tok-1")));
    let instruction = back_model.last_request().unwrap().contents[0].parts[0].clone();
    assert_eq!(instruction.text(), Some("Token: tok-1"));
    assert!(!harness.state().await.contains_key("temp:token"), "temp: keys are not persisted");
}

#[tokio::test]
async fn a_stream_that_ends_on_a_partial_chunk_is_persisted_whole() {
    let model = Arc::new(ScriptedLlm::new(
        "model",
        [
            ScriptedResponse::stream([partial("Hello "), partial("world")]),
            ScriptedResponse::text("again"),
        ],
    ));
    let agent: Arc<dyn Agent> =
        Arc::new(LlmAgentBuilder::new("desk").model(model.clone()).build().unwrap());
    let harness = Harness::new().await;

    let (events, error) = harness.run(agent.clone(), "greet").await;
    assert!(error.is_none(), "{error:?}");
    let complete: Vec<&Event> = events
        .iter()
        .filter(|event| event.author == "desk" && !event.llm_response.partial)
        .collect();
    assert_eq!(complete.len(), 1);
    let streamed = events.iter().find(|event| event.llm_response.partial).unwrap();
    assert_eq!(complete[0].id, streamed.id, "the closing event keeps the stream's event id");

    harness.run(agent, "again").await;
    assert_eq!(
        summarise(&model.requests()[1]),
        vec![pair("user", "greet"), pair("model", "Hello world"), pair("user", "again")]
    );
}

#[tokio::test]
async fn a_turn_without_an_answer_fails_with_model_empty_response() {
    let thinking = LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::Thinking { thinking: "hmm".to_string(), signature: None }],
        }),
        turn_complete: true,
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    };
    let empty = LlmResponse {
        content: None,
        turn_complete: true,
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    };
    for (case, reply) in [
        ("empty", ScriptedResponse::from(empty)),
        ("whitespace", ScriptedResponse::text("  \n ")),
        ("thinking only", ScriptedResponse::from(thinking)),
    ] {
        let model = Arc::new(ScriptedLlm::new("model", [reply]));
        let agent = LlmAgentBuilder::new("desk").model(model).output_key("answer").build().unwrap();
        let harness = Harness::new().await;
        let (_, error) = harness.run(Arc::new(agent), "hello?").await;
        let error = error.unwrap_or_else(|| panic!("{case}: the run must fail"));
        assert_eq!(error.code, "model.empty_response", "{case}");
        assert_eq!(error.details.metadata.get("finish_reason"), Some(&json!("Stop")), "{case}");
        assert!(!harness.state().await.contains_key("answer"), "{case}");
    }
}

#[tokio::test]
async fn an_after_model_replacement_keeps_the_chunk_stream_flags_and_usage() {
    let last = LlmResponse {
        content: None,
        usage_metadata: Some(UsageMetadata { total_token_count: 9, ..Default::default() }),
        turn_complete: true,
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    };
    let model = Arc::new(ScriptedLlm::new(
        "model",
        [ScriptedResponse::stream([partial("Hel"), partial("lo"), last])],
    ));
    let agent = LlmAgentBuilder::new("desk")
        .model(model)
        .after_model_callback(Box::new(|_ctx, response: LlmResponse| {
            Box::pin(async move {
                let Some(content) = response.content else {
                    return Ok(None);
                };
                let text: String = content.parts.iter().filter_map(Part::text).collect();
                Ok(Some(LlmResponse {
                    content: Some(Content::new("model").with_text(text.to_uppercase())),
                    ..Default::default()
                }))
            })
        }))
        .build()
        .unwrap();
    let harness = Harness::new().await;
    let (_, error) = harness.run(Arc::new(agent), "hi").await;
    assert!(error.is_none(), "{error:?}");

    let answers: Vec<Event> =
        harness.persisted().await.into_iter().filter(|event| event.author == "desk").collect();
    assert_eq!(answers.len(), 1, "one persisted event per response");
    let text: String = answers[0]
        .llm_response
        .content
        .iter()
        .flat_map(|content| content.parts.iter())
        .filter_map(Part::text)
        .collect();
    assert_eq!(text, "HELLO");
    assert_eq!(answers[0].llm_response.usage_metadata.as_ref().unwrap().total_token_count, 9);
}

#[tokio::test]
async fn fenced_json_passes_output_schema_validation() {
    let model = Arc::new(ScriptedLlm::new(
        "model",
        [ScriptedResponse::text("```json\n{\"answer\": 1}\n```")],
    ));
    let agent = LlmAgentBuilder::new("desk")
        .model(model.clone())
        .output_schema(json!({
            "type": "object",
            "properties": {"answer": {"type": "integer"}},
            "required": ["answer"],
        }))
        .output_key("result")
        .build()
        .unwrap();
    let harness = Harness::new().await;
    let (events, error) = harness.run(Arc::new(agent), "one?").await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(model.requests().len(), 1, "no correction retries");
    assert_eq!(harness.state().await.get("result"), Some(&json!("{\"answer\": 1}")));
    let typed: Value = adk_agent::extract_typed(&events).unwrap();
    assert_eq!(typed, json!({"answer": 1}));
}

#[tokio::test]
async fn forwarded_agent_tool_events_stay_out_of_the_callers_history() {
    let helper_model =
        Arc::new(ScriptedLlm::new("helper-model", [ScriptedResponse::text("helper says hi")]));
    let helper = LlmAgentBuilder::new("helper").model(helper_model).build().unwrap();
    let desk_model = Arc::new(ScriptedLlm::new(
        "desk-model",
        [
            ScriptedResponse::function_call("helper", json!({"request": "say hi"})),
            ScriptedResponse::text("desk done"),
            ScriptedResponse::text("desk again"),
        ],
    ));
    let desk: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("desk")
            .model(desk_model.clone())
            .tool(Arc::new(AgentTool::new(Arc::new(helper)).forward_events(true)))
            .build()
            .unwrap(),
    );
    let harness = Harness::new().await;
    let (events, error) = harness.run(desk.clone(), "ask the helper").await;
    assert!(error.is_none(), "{error:?}");
    let forwarded: Vec<&Event> = events.iter().filter(|event| event.author == "helper").collect();
    assert!(!forwarded.is_empty());
    assert!(forwarded.iter().all(|event| event.branch.starts_with("helper@")));

    harness.run(desk, "thanks").await;
    assert_eq!(
        summarise(&desk_model.requests()[2]),
        vec![
            pair("user", "ask the helper"),
            pair("model", "call:helper"),
            pair("function", "response:helper"),
            pair("model", "desk done"),
            pair("user", "thanks"),
        ]
    );
}
