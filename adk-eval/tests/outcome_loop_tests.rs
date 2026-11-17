//! `RubricCheckAgent` ends a `LoopAgent` once a grader in its own context passes every
//! criterion, feeds the failed criteria to the next attempt, stops at its cap, and meters its
//! call against the run budget.

use std::sync::Arc;

use adk_agent::{LlmAgentBuilder, LoopAgent};
use adk_core::{Agent, Content, ErrorCategory, Event, RunBudget, RunConfig, SessionId, UserId};
use adk_eval::{RUBRIC_CHECK_OUTPUT_KEY, RubricCheckAgent, RubricConfig};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use serde_json::{Value, json};

fn grade(passed: &[bool]) -> ScriptedResponse {
    let criteria: Vec<Value> = passed
        .iter()
        .enumerate()
        .map(|(index, passed)| {
            json!({"id": index + 1, "passed": passed, "feedback": if *passed { "met" } else { "add the $9 monthly price" }})
        })
        .collect();
    ScriptedResponse::text(json!({"criteria": criteria, "summary": "graded"}).to_string())
}

struct Run {
    events: Vec<adk_core::Result<Event>>,
    state: Option<Value>,
}

async fn run(
    writer: Vec<ScriptedResponse>,
    grader: Vec<ScriptedResponse>,
    max_iterations: u32,
    budget: Option<RunBudget>,
) -> (Run, Arc<ScriptedLlm>, Arc<ScriptedLlm>) {
    let writer_model = Arc::new(ScriptedLlm::new("writer-model", writer));
    let grader_model = Arc::new(ScriptedLlm::new("grader-model", grader));
    let writer = LlmAgentBuilder::new("writer")
        .instruction("Write the product blurb the user asks for.")
        .model(writer_model.clone())
        .output_key("draft")
        .build()
        .unwrap();
    let rubric = RubricConfig::from_criteria(["names the product", "states the monthly price"]);
    let check = RubricCheckAgent::new("rubric_check", grader_model.clone(), rubric)
        .with_deliverable_key("draft")
        .with_max_iterations(max_iterations);
    let agent: Arc<dyn Agent> =
        Arc::new(LoopAgent::new("blurb", vec![Arc::new(writer), Arc::new(check)]));

    let sessions = Arc::new(InMemorySessionService::new());
    let session = sessions
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "alice".into(),
            session_id: None,
            state: Default::default(),
        })
        .await
        .unwrap();
    let mut builder =
        Runner::builder().app_name("app").agent(agent).session_service(sessions.clone());
    if let Some(budget) = budget {
        builder = builder.run_config(RunConfig::builder().budget(budget).build());
    }
    let runner = builder.build().unwrap();
    let stream = runner
        .run(
            UserId::try_from("alice").unwrap(),
            SessionId::try_from(session.id()).unwrap(),
            Content::new("user").with_text("A blurb for Ferris Notes"),
        )
        .await
        .unwrap();
    let events: Vec<_> = stream.collect().await;
    let state = sessions
        .get(GetRequest {
            app_name: "app".into(),
            user_id: "alice".into(),
            session_id: session.id().to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap()
        .state()
        .get(RUBRIC_CHECK_OUTPUT_KEY);
    (Run { events, state }, writer_model, grader_model)
}

#[tokio::test]
async fn the_loop_revises_on_feedback_and_stops_once_the_rubric_passes() {
    let (run, writer, grader) = run(
        vec![
            ScriptedResponse::text("Ferris Notes: notes."),
            ScriptedResponse::text("Ferris Notes, $9 a month."),
        ],
        vec![grade(&[true, false]), grade(&[true, true])],
        3,
        None,
    )
    .await;
    assert!(run.events.iter().all(Result::is_ok));
    let state = run.state.unwrap();
    assert_eq!(
        (state["iteration"].clone(), state["outcome"].clone()),
        (json!(2), json!("satisfied"))
    );
    assert_eq!(writer.remaining(), 0);
    assert_eq!(grader.remaining(), 0);

    // The grader saw one message: the task, the criteria and the draft, no conversation.
    let first = &grader.requests()[0];
    assert_eq!(first.contents.len(), 1);
    let asked = serde_json::to_string(&first.contents).unwrap();
    assert!(asked.contains("A blurb for Ferris Notes"), "{asked}");
    assert!(asked.contains("2. states the monthly price"), "{asked}");
    assert!(asked.contains("Ferris Notes: notes."), "{asked}");
    assert!(!asked.contains("Write the product blurb"), "the writer's instruction leaked: {asked}");

    // The writer's second attempt read the failed criterion and its feedback.
    let second = serde_json::to_string(&writer.requests()[1].contents).unwrap();
    assert!(second.contains("FAILED: states the monthly price"), "{second}");
    assert!(second.contains("add the $9 monthly price"), "{second}");
}

#[tokio::test]
async fn the_loop_stops_at_the_cap_with_a_clear_outcome() {
    let (run, writer, _grader) = run(
        vec![ScriptedResponse::text("draft one"), ScriptedResponse::text("draft two")],
        vec![grade(&[true, false]), grade(&[true, false])],
        2,
        None,
    )
    .await;
    let state = run.state.unwrap();
    assert_eq!(
        (state["iteration"].clone(), state["outcome"].clone()),
        (json!(2), json!("max_iterations_reached"))
    );
    assert_eq!(state["grade"]["criteria"][1]["passed"], json!(false));
    assert_eq!(writer.remaining(), 0, "no third attempt");
    let last = run.events.last().unwrap().as_ref().unwrap();
    let text: String =
        last.content().unwrap().parts.iter().filter_map(|part| part.text()).collect();
    assert!(
        text.starts_with("[System notice] Rubric not satisfied after 2 attempts, the most allowed"),
        "{text}"
    );
    assert!(adk_core::history::is_system_notice_event(last));
    assert!(last.actions.escalate);
}

#[tokio::test]
async fn the_graders_call_counts_against_the_run_budget() {
    let (run, _writer, grader) = run(
        vec![ScriptedResponse::text("draft one")],
        vec![grade(&[true, true])],
        3,
        Some(RunBudget::new().max_model_calls(1)),
    )
    .await;
    let error = run.events.iter().find_map(|event| event.as_ref().err()).expect("the run stops");
    assert_eq!(error.category, ErrorCategory::ResourceExhausted, "{error}");
    assert!(grader.requests().is_empty(), "the refused call never reached the model");
}

#[tokio::test]
async fn a_cap_above_twenty_is_refused() {
    let (run, _writer, grader) =
        run(vec![ScriptedResponse::text("draft")], vec![grade(&[true, true])], 21, None).await;
    let error = run.events.iter().find_map(|event| event.as_ref().err()).expect("refused");
    assert_eq!(error.code, "eval.outcome.invalid_cap");
    assert!(grader.requests().is_empty());
}
