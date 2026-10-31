//! The plan graph (spec 3.1 §7.11, H4).

#![cfg(all(feature = "graph", feature = "runner"))]

use crate::common;

use adk_harness::{
    HarnessDecision, HarnessMode, PLAN_GATE_TOOL, PlanGate, PlanGraphShape, PlanRunStatus,
    PlanStep, compile_plan,
};
use adk_model::ScriptedResponse;
use common::*;
use serde_json::{Value, json};

fn call(name: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call(name, args)
}

fn text(text: &str) -> ScriptedResponse {
    ScriptedResponse::text(text)
}

fn step(id: &str, deps: &[&str], gate: PlanGate) -> PlanStep {
    PlanStep {
        id: id.into(),
        title: format!("Step {id}"),
        instruction: format!("Write {id}.txt"),
        depends_on: deps.iter().map(|dep| (*dep).to_string()).collect(),
        gate,
    }
}

fn edge(from: &str, to: &str) -> (String, String) {
    (from.to_string(), to.to_string())
}

#[test]
fn the_plan_compiler_produces_the_expected_graph_shape() {
    // 1 → {2 (review), 3} → 4 (approve)
    let steps = vec![
        step("1", &[], PlanGate::None),
        step("2", &["1"], PlanGate::Review),
        step("3", &["1"], PlanGate::None),
        step("4", &["2", "3"], PlanGate::Approve),
    ];
    let shape = compile_plan(&steps, "hash").unwrap();
    let mut edges = vec![
        edge("__start__", "step:1"),
        edge("step:1", "step:2"),
        edge("step:1", "step:3"),
        edge("step:2", "review:2"),
        edge("review:2", "step:4"),
        edge("step:3", "step:4"),
        edge("step:4", "__end__"),
    ];
    edges.sort();
    assert_eq!(
        shape,
        PlanGraphShape {
            plan_hash: "hash".into(),
            nodes: vec!["step:1", "step:2", "review:2", "step:3", "step:4"]
                .into_iter()
                .map(String::from)
                .collect(),
            edges,
            interrupt_before: vec!["step:4".to_string()],
            recursion_limit: 6,
        }
    );
    let schema = shape.workflow_schema();
    assert_eq!(schema.agent_nodes.len(), 5);
    assert_eq!(schema.edges.len(), 7);
}

#[test]
fn invalid_plans_do_not_compile() {
    assert!(compile_plan(&[], "h").is_err(), "no steps");
    assert!(compile_plan(&[step("1", &["2"], PlanGate::None)], "h").is_err(), "unknown dep");
    assert!(compile_plan(&[step("1", &["1"], PlanGate::None)], "h").is_err(), "self dep");
    assert!(
        compile_plan(&[step("1", &["2"], PlanGate::None), step("2", &["1"], PlanGate::None)], "h")
            .is_err(),
        "cycle"
    );
    assert!(
        compile_plan(&[step("1", &[], PlanGate::None), step("1", &[], PlanGate::None)], "h")
            .is_err(),
        "duplicate id"
    );
}

fn plan_call(steps: &[PlanStep]) -> ScriptedResponse {
    call(
        "exit_plan_mode",
        json!({
            "plan": "Write three files.",
            "next_mode": "auto",
            "steps": steps,
        }),
    )
}

fn write(path: &str) -> ScriptedResponse {
    call("write_file", json!({ "path": path, "content": path }))
}

fn three_steps(gate: PlanGate) -> Vec<PlanStep> {
    vec![step("1", &[], PlanGate::None), step("2", &["1"], PlanGate::None), step("3", &["2"], gate)]
}

/// A harness in Plan mode whose model plans `steps`, approved through a prompter.
async fn planned(steps: &[PlanStep], after: Vec<ScriptedResponse>) -> Fixture {
    let mut script = vec![plan_call(steps)];
    script.extend(after);
    let prompter = ScriptedPrompter::new();
    prompter.approve_once();
    let f = fixture(script, move |b| {
        b.mode(HarnessMode::Plan).without_default_members().prompter(prompter)
    })
    .await;
    let events = f.send("plan the change").await;
    assert_eq!(
        response_of(&events, "exit_plan_mode")["status"],
        "approved",
        "{:#?}",
        responses(&events)
    );
    f
}

/// The plan-step instructions recorded in the session, in order.
async fn instructions_seen(f: &Fixture) -> Vec<String> {
    let session = f
        .harness
        .runner()
        .session_service()
        .get(adk_session::GetRequest {
            app_name: APP.into(),
            user_id: f.user.to_string(),
            session_id: f.session.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    session
        .events()
        .all()
        .into_iter()
        .filter(|event| event.author == "user")
        .filter_map(|event| event.llm_response.content)
        .filter_map(|content| content.parts.iter().find_map(|part| part.text().map(String::from)))
        .filter(|text| text.starts_with("[plan step"))
        .collect()
}

#[tokio::test]
async fn an_approved_plan_runs_step_by_step_and_its_approve_gate_waits_for_a_decision() {
    let f = planned(
        &three_steps(PlanGate::Approve),
        vec![
            write("1.txt"),
            text("step 1 done"),
            write("2.txt"),
            text("step 2 done"),
            write("3.txt"),
            text("step 3 done"),
        ],
    )
    .await;
    let compiled = f.records("harness.plan.compiled");
    assert_eq!(compiled.len(), 1);
    assert_eq!(compiled[0].ext["interruptBefore"], json!(["step:3"]));

    // Steps 1 and 2 run; the gate before step 3 holds the turn.
    let events = f.send("go").await;
    assert!(f.path("1.txt").exists() && f.path("2.txt").exists());
    assert!(!f.path("3.txt").exists());
    let held = confirmation_requests(&events);
    assert_eq!(held.len(), 1, "{held:#?}");
    assert_eq!(held[0].tool_name, PLAN_GATE_TOOL);
    assert_eq!(held[0].args["stepId"], "3");
    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].fingerprint, held[0].fingerprint());

    // Another turn without a decision does not pass the gate.
    let events = f.send("continue").await;
    assert!(!f.path("3.txt").exists());
    assert!(confirmation_requests(&events).is_empty(), "the gate is still held, not held twice");

    // A remembered rule cannot cover a gate: it is decided once.
    let remember = f
        .harness
        .decide(
            &f.user,
            &f.session,
            &pending[0].fingerprint,
            HarnessDecision::ApproveAndRemember(adk_harness::RuleSuggestion {
                label: "always".into(),
                tool: PLAN_GATE_TOOL.into(),
                origin: adk_harness::OriginPin::AnyDeclaration { source: "static".into() },
                predicates: Vec::new(),
                scope: adk_harness::SuggestedScope::Session,
                runs_scripts: false,
            }),
        )
        .await;
    assert!(remember.is_err());
    f.harness
        .decide(&f.user, &f.session, &pending[0].fingerprint, HarnessDecision::Approve)
        .await
        .unwrap();
    f.resume().await;
    assert!(f.path("3.txt").exists());
    let graphs = f.harness.agent().runtime().plan_graph().unwrap().clone();
    let plan = graphs.active(APP, f.user.as_ref(), f.session.as_ref()).await.unwrap().unwrap();
    assert_eq!(plan.status, PlanRunStatus::Done);
    let gates: Vec<Value> = f
        .records("harness.plan.step")
        .iter()
        .map(|record| record.ext["decision"].clone())
        .collect();
    assert_eq!(gates, vec![json!("pending"), json!("approved")]);
    let seen = instructions_seen(&f).await;
    assert_eq!(seen.len(), 3, "each step ran once: {seen:#?}");

    // A finished plan no longer drives turns.
    f.model.push(text("free-form"));
    let events = f.send("thanks").await;
    assert!(events.iter().any(|event| event.author == "harness"));
}

#[tokio::test]
async fn a_denied_gate_stops_the_plan() {
    let f = planned(
        &three_steps(PlanGate::Approve),
        vec![write("1.txt"), text("1"), write("2.txt"), text("2")],
    )
    .await;
    f.send("go").await;
    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    f.harness
        .decide(
            &f.user,
            &f.session,
            &pending[0].fingerprint,
            HarnessDecision::Deny { message: Some("not now".into()) },
        )
        .await
        .unwrap();
    f.resume().await;
    assert!(!f.path("3.txt").exists());
    let graphs = f.harness.agent().runtime().plan_graph().unwrap().clone();
    let plan = graphs.active(APP, f.user.as_ref(), f.session.as_ref()).await.unwrap().unwrap();
    assert_eq!(plan.status, PlanRunStatus::Cancelled);
}

#[tokio::test]
async fn plan_graph_rewind_re_executes_nothing_before_the_step() {
    // spec 3.1 §5 H4 and §11: step 3 fails; rewind to step 2; steps 1 and 2 never run again.
    let f = planned(
        &three_steps(PlanGate::None),
        vec![
            write("1.txt"),
            text("1"),
            write("2.txt"),
            text("2"),
            ScriptedResponse::error(adk_core::AdkError::model("the provider is down")),
        ],
    )
    .await;
    let stream = f.harness.send(&f.user, &f.session, "go").await.unwrap();
    let results: Vec<_> = futures::StreamExt::collect(stream).await;
    assert!(results.iter().any(Result::is_err), "step 3 fails the turn");
    assert!(f.path("1.txt").exists() && f.path("2.txt").exists() && !f.path("3.txt").exists());

    let graphs = f.harness.agent().runtime().plan_graph().unwrap().clone();
    let thread = graphs.rewind(APP, f.user.as_ref(), f.session.as_ref(), "2").await.unwrap();
    assert!(thread.ends_with(":rewind:1"), "{thread}");
    std::fs::remove_file(f.path("1.txt")).unwrap();
    std::fs::remove_file(f.path("2.txt")).unwrap();

    f.model.push(write("3.txt"));
    f.model.push(text("3"));
    f.send("again").await;
    assert!(f.path("3.txt").exists());
    assert!(!f.path("1.txt").exists() && !f.path("2.txt").exists(), "steps 1 and 2 did not run");
    let seen = instructions_seen(&f).await;
    assert_eq!(
        seen.iter().filter(|text| text.starts_with("[plan step 1:")).count(),
        1,
        "{seen:#?}"
    );
    assert_eq!(
        seen.iter().filter(|text| text.starts_with("[plan step 2:")).count(),
        1,
        "{seen:#?}"
    );
    assert_eq!(
        seen.iter().filter(|text| text.starts_with("[plan step 3:")).count(),
        2,
        "{seen:#?}"
    );
    assert!(seen.last().unwrap().contains("run 1"), "{seen:#?}");
    let plan = graphs.active(APP, f.user.as_ref(), f.session.as_ref()).await.unwrap().unwrap();
    assert_eq!(plan.status, PlanRunStatus::Done);
}

#[tokio::test]
async fn an_invalid_plan_is_refused_and_the_mode_stays() {
    let f = fixture(
        vec![
            call(
                "exit_plan_mode",
                json!({
                    "plan": "loop",
                    "steps": [step("1", &["2"], PlanGate::None), step("2", &["1"], PlanGate::None)],
                }),
            ),
            text("ok"),
        ],
        |b| {
            let prompter = ScriptedPrompter::new();
            prompter.approve_once();
            b.mode(HarnessMode::Plan).without_default_members().prompter(prompter)
        },
    )
    .await;
    let events = f.send("plan").await;
    let response = response_of(&events, "exit_plan_mode");
    assert!(response.to_string().contains("harness.plan.invalid"), "{response}");
    assert_eq!(f.harness.mode(&f.user, &f.session).await.unwrap(), HarnessMode::Plan);
}

#[cfg(feature = "team")]
#[tokio::test]
async fn a_review_gate_runs_the_reviewer_member_after_the_step() {
    let members = std::sync::Arc::new(adk_model::ScriptedLlm::new(
        "member",
        vec![text("LGTM: 2.txt looks right")],
    ));
    let member_model = members.clone();
    let steps = vec![step("1", &[], PlanGate::None), step("2", &["1"], PlanGate::Review)];
    let prompter = ScriptedPrompter::new();
    prompter.approve_once();
    let f = fixture(
        vec![plan_call(&steps), write("1.txt"), text("1"), write("2.txt"), text("2")],
        move |b| b.mode(HarnessMode::Plan).member_model(member_model).prompter(prompter),
    )
    .await;
    f.send("plan").await;
    let events = f.send("go").await;
    assert!(f.path("2.txt").exists());
    assert_eq!(members.remaining(), 0, "the reviewer (explore) ran");
    let reviewed: Vec<Value> = f
        .records("harness.plan.step")
        .iter()
        .map(|record| record.ext["decision"].clone())
        .collect();
    assert_eq!(reviewed, vec![json!("reviewed")]);
    assert_eq!(f.records("harness.member.spawned")[0].ext["member"], "explore");
    assert!(
        events.iter().any(|event| event.author == "explore"),
        "the review's events reach the session"
    );
}
