//! The goal engine (spec 3.1 §7.11).

#![cfg(feature = "runner")]

use crate::common;

use std::time::Duration;

use adk_harness::{GOAL_STATE_KEY, GoalRubric, GoalSpec, GoalStatus, HarnessMode};
use adk_model::ScriptedResponse;
use common::*;
use serde_json::{Value, json};

fn call(name: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call(name, args)
}

fn text(text: &str) -> ScriptedResponse {
    ScriptedResponse::text(text)
}

fn write(path: &str) -> ScriptedResponse {
    call("write_file", json!({ "path": path, "content": path }))
}

fn goal(max_iterations: u32) -> GoalSpec {
    GoalSpec::new("create done.txt")
        .with_until("test -f done.txt")
        .with_max_iterations(max_iterations)
        .with_check_timeout(Duration::from_secs(30))
}

/// Two iterations: the first writes progress.txt (the check fails), the second done.txt.
fn two_iterations() -> Vec<ScriptedResponse> {
    vec![write("progress.txt"), text("not yet"), write("done.txt"), text("done")]
}

async fn run_goal(f: &Fixture, spec: GoalSpec) -> Vec<adk_core::Event> {
    let stream = f.harness.start_goal(&f.user, &f.session, spec).await.unwrap();
    collect(stream).await
}

fn statuses(f: &Fixture) -> Vec<Value> {
    f.records("harness.goal.iteration").iter().map(|record| record.ext["status"].clone()).collect()
}

#[tokio::test]
async fn a_goal_iterates_until_its_check_passes() {
    let f =
        fixture(two_iterations(), |b| b.mode(HarnessMode::Auto).without_default_members()).await;
    let events = run_goal(&f, goal(5)).await;
    let state = f.harness.goal(&f.user, &f.session).await.unwrap().unwrap();
    assert_eq!(state.status, GoalStatus::Done);
    assert_eq!(state.iteration, 2);
    assert_eq!(state.last_check, Some(0));
    assert_eq!(statuses(&f), vec![json!("running"), json!("done")]);
    // The check is the operator's command, audited as a goal check.
    let begins = f.records("harness.bash.begin");
    assert_eq!(begins.len(), 2);
    assert!(begins.iter().all(|record| record.ext["goalCheck"] == true));
    // The second iteration read the first check's failure.
    let seen = serde_json::to_string(&f.model.requests()[2].contents).unwrap();
    assert!(seen.contains("failed (exit 1) on iteration 1 of 5"), "{seen}");
    // The state is mirrored for display.
    let mirrored = events
        .iter()
        .filter_map(|event| event.actions.state_delta.get(GOAL_STATE_KEY))
        .next_back()
        .cloned()
        .unwrap();
    assert_eq!(mirrored["status"], "done");
    // A finished goal no longer drives the session's turns.
    f.model.push(text("free"));
    f.send("thanks").await;
    assert_eq!(f.harness.goal(&f.user, &f.session).await.unwrap().unwrap().iteration, 2);
}

#[tokio::test]
async fn each_check_runs_in_a_harness_goal_span_with_its_iteration() {
    let log = SpanLog::default();
    let _guard = log.install();
    let f =
        fixture(two_iterations(), |b| b.mode(HarnessMode::Auto).without_default_members()).await;
    run_goal(&f, goal(5)).await;
    let lines = log.lines();
    assert_eq!(
        lines.iter().filter(|line| line.starts_with("harness.goal")).count(),
        2,
        "{lines:#?}"
    );
    assert!(lines.iter().any(|line| line.contains("harness.goal.iteration=2")), "{lines:#?}");
}

#[tokio::test]
async fn a_goal_stops_at_its_iteration_cap() {
    let f = fixture(vec![write("a.txt"), text("1"), write("b.txt"), text("2")], |b| {
        b.mode(HarnessMode::Auto).without_default_members()
    })
    .await;
    run_goal(&f, goal(2)).await;
    let state = f.harness.goal(&f.user, &f.session).await.unwrap().unwrap();
    assert_eq!(state.status, GoalStatus::Exhausted);
    assert_eq!(state.iteration, 2);
    assert_eq!(statuses(&f), vec![json!("running"), json!("exhausted")]);
    assert_eq!(f.model.remaining(), 0, "no third iteration");
}

/// The grader's reply: criterion `i + 1` passes when `passed[i]`.
fn grade(passed: &[bool]) -> ScriptedResponse {
    let criteria: Vec<Value> = passed
        .iter()
        .enumerate()
        .map(|(index, passed)| {
            json!({
                "id": index + 1,
                "passed": passed,
                "feedback": if *passed { "met" } else { "state the monthly price, $9" },
            })
        })
        .collect();
    text(&json!({ "criteria": criteria, "summary": "graded" }).to_string())
}

fn blurb_rubric() -> GoalRubric {
    GoalRubric::new(["names the product", "states the monthly price"])
        .with_deliverables(["blurb.md"])
}

fn graded_goal(rubric: GoalRubric) -> GoalSpec {
    GoalSpec::new("write the launch blurb to blurb.md").with_rubric(rubric)
}

#[tokio::test]
async fn a_rubric_goal_revises_on_the_graders_feedback_until_every_criterion_passes() {
    let script = vec![
        write("blurb.md"),
        text("first draft"),
        grade(&[true, false]),
        write("blurb.md"),
        text("second draft"),
        grade(&[true, true]),
    ];
    let f = fixture(script, |b| b.mode(HarnessMode::Auto).without_default_members()).await;
    run_goal(&f, graded_goal(blurb_rubric())).await;
    let state = f.harness.goal(&f.user, &f.session).await.unwrap().unwrap();
    assert_eq!((state.status, state.iteration, state.evaluations), (GoalStatus::Done, 2, 2));
    assert!(state.last_grade.as_ref().is_some_and(adk_eval::OutcomeGrade::satisfied));
    assert_eq!(state.outcome().unwrap(), "goal met after 2 iteration(s): the rubric passed.");
    // A rubric alone runs no command.
    assert!(f.records("harness.bash.begin").is_empty());
    // Each grading round is audited.
    let grades = f.records("harness.goal.grade");
    let verdicts: Vec<(Value, Value)> = grades
        .iter()
        .map(|record| (record.ext["satisfied"].clone(), record.ext["failed"].clone()))
        .collect();
    assert_eq!(verdicts, vec![(json!(false), json!([2])), (json!(true), json!([]))]);

    let requests = f.model.requests();
    // The grader works in a context of its own: one message holding the goal, the
    // criteria, the final message and the deliverable file, not the worker's conversation.
    let grader = &requests[2];
    assert_eq!(grader.contents.len(), 1, "{:?}", grader.contents);
    let asked = serde_json::to_string(&grader.contents).unwrap();
    for expected in
        ["write the launch blurb", "2. states the monthly price", "first draft", "File `blurb.md`"]
    {
        assert!(asked.contains(expected), "missing {expected:?} in {asked}");
    }
    assert!(!asked.contains("Work toward the goal"), "the worker's prompt leaked: {asked}");
    assert!(grader.config.as_ref().is_some_and(|config| config.response_schema.is_some()));
    // The second iteration read the grader's feedback.
    let revised = serde_json::to_string(&requests[3].contents).unwrap();
    // As the harness's own notice, not another agent's output quoted as data.
    assert!(revised.contains("[System notice] The grader failed 1 of 2 criteria"), "{revised}");
    assert!(!revised.contains("[until_check] said"), "{revised}");
    assert!(revised.contains("FAILED: states the monthly price"), "{revised}");
    assert!(revised.contains("state the monthly price, $9"), "{revised}");
}

#[tokio::test]
async fn a_rubric_goal_stops_once_its_grading_rounds_are_used_up() {
    let script = vec![
        write("blurb.md"),
        text("first draft"),
        grade(&[true, false]),
        write("blurb.md"),
        text("second draft"),
        grade(&[false, false]),
    ];
    let f = fixture(script, |b| b.mode(HarnessMode::Auto).without_default_members()).await;
    let spec = graded_goal(blurb_rubric().with_max_evaluations(2)).with_max_iterations(10);
    run_goal(&f, spec).await;
    let state = f.harness.goal(&f.user, &f.session).await.unwrap().unwrap();
    assert_eq!((state.status, state.iteration, state.evaluations), (GoalStatus::Exhausted, 2, 2));
    assert_eq!(f.model.remaining(), 0, "no third iteration");
    let outcome = state.outcome().unwrap();
    assert!(
        outcome.starts_with(
            "budget exhausted after 2 iteration(s); the rubric is not satisfied after 2 grading \
             round(s): 2 of 2 criteria fail"
        ),
        "{outcome}"
    );
    assert!(outcome.contains("- FAILED: names the product"), "{outcome}");
    assert_eq!(statuses(&f), vec![json!("running"), json!("exhausted")]);
}

#[tokio::test]
async fn with_until_the_grader_runs_only_once_the_command_passes() {
    let script = vec![
        write("progress.txt"),
        text("not yet"),
        write("done.txt"),
        text("done"),
        grade(&[true]),
    ];
    let f = fixture(script, |b| b.mode(HarnessMode::Auto).without_default_members()).await;
    let spec = goal(5).with_rubric(GoalRubric::new(["the final message says done"]));
    run_goal(&f, spec).await;
    let state = f.harness.goal(&f.user, &f.session).await.unwrap().unwrap();
    assert_eq!((state.status, state.iteration, state.evaluations), (GoalStatus::Done, 2, 1));
    assert_eq!(f.records("harness.bash.begin").len(), 2);
    assert_eq!(f.records("harness.goal.grade").len(), 1, "no grading while the command fails");
    assert_eq!(f.model.remaining(), 0);
}

#[tokio::test]
async fn the_graders_call_counts_against_the_run_budget() {
    let script = vec![write("blurb.md"), text("first draft"), grade(&[true, true])];
    let f = fixture(script, |b| {
        b.mode(HarnessMode::Auto)
            .without_default_members()
            .budget(adk_core::RunBudget::new().max_model_calls(2))
    })
    .await;
    let stream =
        f.harness.start_goal(&f.user, &f.session, graded_goal(blurb_rubric())).await.unwrap();
    let results: Vec<_> = futures::StreamExt::collect(stream).await;
    let error = results.iter().find_map(|result| result.as_ref().err()).expect("the run stops");
    assert_eq!(error.category, adk_core::ErrorCategory::ResourceExhausted, "{error}");
    assert_eq!(f.model.requests().len(), 2, "the refused grading call never reached the model");
    assert_eq!(f.model.remaining(), 1);
    assert!(f.records("harness.goal.grade").is_empty());
}

#[tokio::test]
async fn the_graph_engine_grades_the_iterations_final_message_too() {
    let script = vec![
        write("blurb.md"),
        text("first draft"),
        grade(&[false, true]),
        write("blurb.md"),
        text("second draft"),
        grade(&[true, true]),
    ];
    let f = fixture(script, |b| {
        b.mode(HarnessMode::Auto)
            .without_default_members()
            .goal_engine(adk_harness::GoalEngine::Graph)
    })
    .await;
    run_goal(&f, graded_goal(blurb_rubric())).await;
    let state = f.harness.goal(&f.user, &f.session).await.unwrap().unwrap();
    assert_eq!((state.status, state.evaluations), (GoalStatus::Done, 2));
    let requests = f.model.requests();
    let second_grading = serde_json::to_string(&requests[5].contents).unwrap();
    assert!(second_grading.contains("second draft"), "{second_grading}");
    assert!(!second_grading.contains("first draft"), "{second_grading}");
}

#[tokio::test]
async fn a_hidden_rubric_reaches_the_grader_only() {
    let script = vec![write("blurb.md"), text("draft"), grade(&[true, true])];
    let f = fixture(script, |b| b.mode(HarnessMode::Auto).without_default_members()).await;
    run_goal(&f, graded_goal(blurb_rubric().hidden(true))).await;
    let requests = f.model.requests();
    let worker = serde_json::to_string(&requests[0].contents).unwrap();
    assert!(!worker.contains("states the monthly price"), "{worker}");
    assert!(worker.contains("criteria you are not shown"), "{worker}");
    let grader = serde_json::to_string(&requests[2].contents).unwrap();
    assert!(grader.contains("states the monthly price"), "{grader}");
}

#[tokio::test]
async fn goals_require_ask_or_auto() {
    for mode in [HarnessMode::Plan, HarnessMode::ReadOnly] {
        let f = fixture(Vec::new(), move |b| b.mode(mode).without_default_members()).await;
        let error = f.harness.start_goal(&f.user, &f.session, goal(3)).await.err().unwrap();
        assert_eq!(error.code, "harness.goal.mode");
        assert!(f.harness.goal(&f.user, &f.session).await.unwrap().is_none());
    }
    let f = fixture(Vec::new(), |b| b.without_default_members()).await;
    let incomplete = goal(3).with_until("");
    let error = f.harness.start_goal(&f.user, &f.session, incomplete).await.err().unwrap();
    assert_eq!(error.code, "harness.goal.invalid");
}

#[tokio::test]
async fn a_goal_iteration_is_governed_by_the_session_mode() {
    // In Ask, with no one to approve, the iteration's write is held and the loop pauses
    // before the check: the goal does not advance.
    let f = fixture(vec![write("done.txt"), text("waiting")], |b| {
        b.mode(HarnessMode::Ask).without_default_members()
    })
    .await;
    let events = run_goal(&f, goal(3)).await;
    assert_eq!(outcome(&response_of(&events, "write_file")), Outcome::Held);
    assert!(!f.path("done.txt").exists());
    let state = f.harness.goal(&f.user, &f.session).await.unwrap().unwrap();
    assert_eq!((state.status, state.iteration), (GoalStatus::Running, 0));

    // The decision resumes the held call and the loop at the same iteration.
    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    f.harness
        .decide(&f.user, &f.session, &pending[0].fingerprint, adk_harness::HarnessDecision::Approve)
        .await
        .unwrap();
    f.model.push(text("written"));
    f.resume().await;
    let state = f.harness.goal(&f.user, &f.session).await.unwrap().unwrap();
    assert_eq!((state.status, state.iteration), (GoalStatus::Done, 1));
}

#[cfg(feature = "graph")]
#[tokio::test]
async fn the_graph_engine_iterates_on_checkpoints() {
    let checkpointer: std::sync::Arc<dyn adk_graph::Checkpointer> =
        std::sync::Arc::new(adk_graph::MemoryCheckpointer::new());
    let probe = checkpointer.clone();
    let f = fixture(two_iterations(), move |b| {
        b.mode(HarnessMode::Auto).without_default_members().plan_checkpointer(checkpointer)
    })
    .await;
    run_goal(&f, goal(5)).await;
    let state = f.harness.goal(&f.user, &f.session).await.unwrap().unwrap();
    assert_eq!((state.status, state.iteration), (GoalStatus::Done, 2));
    // iterate, check, iterate, check: a checkpoint per super-step on the goal's thread.
    let record = f
        .harness
        .agent()
        .runtime()
        .goal_store()
        .get(APP, f.user.as_ref(), f.session.as_ref())
        .await
        .unwrap()
        .unwrap();
    let thread = format!(
        "{}:goal:{}",
        adk_graph::agent::session_thread_id(APP, f.user.as_ref(), f.session.as_ref()),
        record.run_id
    );
    assert!(probe.list(&thread).await.unwrap().len() >= 4);
    assert_eq!(statuses(&f), vec![json!("running"), json!("done")]);
}

#[cfg(feature = "graph")]
#[tokio::test]
async fn the_loop_engine_is_the_default_and_graph_when_a_checkpointer_is_set() {
    let f = fixture(two_iterations(), |b| {
        b.mode(HarnessMode::Auto)
            .without_default_members()
            .goal_engine(adk_harness::GoalEngine::Graph)
    })
    .await;
    run_goal(&f, goal(5)).await;
    assert_eq!(
        f.harness.goal(&f.user, &f.session).await.unwrap().unwrap().status,
        GoalStatus::Done
    );
}

#[cfg(feature = "ambient")]
mod ambient {
    use super::*;
    use std::sync::Arc;

    /// Fires once, like a file change or a cron tick.
    struct Once;

    #[async_trait::async_trait]
    impl adk_agent::EventSource for Once {
        fn name(&self) -> &str {
            "file:Cargo.lock"
        }

        async fn subscribe(
            &self,
        ) -> adk_core::Result<futures::stream::BoxStream<'static, adk_agent::TriggerEvent>>
        {
            Ok(Box::pin(futures::stream::iter(vec![adk_agent::TriggerEvent::new(
                "file:Cargo.lock",
                json!({ "path": "Cargo.lock" }),
            )])))
        }
    }

    #[tokio::test]
    async fn a_trigger_starts_a_goal_under_the_same_policy() {
        let f = fixture(vec![write("done.txt"), text("done")], |b| {
            b.mode(HarnessMode::Auto).without_default_members()
        })
        .await;
        let (user, session) = (f.user.clone(), f.session.clone());
        let Fixture { harness, dir, .. } = f;
        let harness = Arc::new(harness);
        let mut ambient = harness.clone().ambient_goal(
            Arc::new(Once),
            user.clone(),
            Some(session.clone()),
            goal(3),
        );
        let mut outputs = ambient.take_output(64);
        ambient.start().await.unwrap();
        let mut done = false;
        for _ in 0..200 {
            while let Ok(output) = outputs.try_recv() {
                output.unwrap();
            }
            if harness.goal(&user, &session).await.unwrap().map(|state| state.status)
                == Some(GoalStatus::Done)
            {
                done = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        ambient.stop().await.unwrap();
        assert!(done, "the triggered goal finished");
        assert!(dir.path().join("done.txt").exists());
    }
}

/// The goal's `check_timeout` bounds its check (spec 3.1 §7.11), even above the workspace's
/// shell limit for the model's own commands.
#[tokio::test]
async fn the_check_runs_for_its_check_timeout_beyond_the_shell_limit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let f = fixture_in(dir, vec![text("waiting")], move |b| {
        b.workspace(
            adk_devtools::Workspace::new(&root)
                .allow_bash(true)
                .bash_timeout(Duration::from_secs(1)),
        )
        .mode(HarnessMode::Auto)
        .without_default_members()
    })
    .await;
    let spec = GoalSpec::new("wait for the slow check")
        .with_until("sleep 2 && touch checked")
        .with_max_iterations(1)
        .with_check_timeout(Duration::from_secs(20));
    run_goal(&f, spec).await;
    let state = f.harness.goal(&f.user, &f.session).await.unwrap().unwrap();
    assert_eq!((state.status, state.last_check), (GoalStatus::Done, Some(0)));
    assert!(f.dir.path().join("checked").exists());
}
