//! A goal and a server's background runs on one run store (spec 3.1 §7.11, §8.2): the goal
//! worker of `adk-rust goal` claims only goal runs, and a served run stays for its own worker.

#![cfg(feature = "harness")]

use std::sync::Arc;
use std::time::Duration;

use adk_cli::console::ConsoleIo;
use adk_cli::harness::HarnessLauncher;
use adk_core::{DurableRunStore, Enqueued, InMemoryRunStore, NewRun, RunFilter, RunState};
use adk_harness::{
    GOAL_QUEUE, GOAL_TARGET, GoalSpec, GoalStatus, HarnessAgent, HarnessMode, InstructionsConfig,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_session::InMemorySessionService;
use serde_json::json;

#[tokio::test]
async fn goal_runs_and_served_runs_on_one_store_stay_with_their_own_workers() {
    let dir = tempfile::tempdir().unwrap();
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    // A background run of the served harness, queued as `adk-rust serve` queues them.
    let served = match runs
        .enqueue(NewRun::new("default", "adk-rust", "user", "adk-rust", json!({ "text": "hi" })))
        .await
        .unwrap()
    {
        Enqueued::Created(run) => run,
        other => panic!("not enqueued: {other:?}"),
    };

    let model = ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "done.txt", "content": "ok" }),
            ),
            ScriptedResponse::text("Done."),
        ],
    );
    let agent = HarnessAgent::builder("adk-rust")
        .model(Arc::new(model))
        .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
        .project_instructions(InstructionsConfig {
            user_file: None,
            ..InstructionsConfig::default()
        })
        .mode(HarnessMode::Auto)
        .without_default_members()
        .build()
        .unwrap();
    let mut launcher = HarnessLauncher::new(agent)
        .with_session_service(Arc::new(InMemorySessionService::new()))
        .with_run_store(runs.clone())
        .with_console(ConsoleIo::scripted([], false))
        .quiet();
    let outcome = launcher
        .run_goal(GoalSpec {
            goal: "create done.txt".into(),
            until: "test -f done.txt".into(),
            max_iterations: 2,
            check_timeout: Duration::from_secs(30),
        })
        .await
        .unwrap();
    assert!(outcome.held.is_empty(), "{outcome:?}");
    assert_eq!(launcher.goal().await.unwrap().map(|state| state.status), Some(GoalStatus::Done));

    // The goal worker left the served run alone, and the goal ran on the goal queue.
    assert_eq!(runs.get(&served.run_id).await.unwrap().unwrap().state, RunState::Queued);
    let goal_runs = runs.list(RunFilter::default().queue(GOAL_QUEUE)).await.unwrap();
    assert_eq!(goal_runs.len(), 1, "{goal_runs:?}");
    assert_eq!(
        (goal_runs[0].target.as_str(), goal_runs[0].state),
        (GOAL_TARGET, RunState::Completed)
    );
    // A worker on the server's queue finds only the served run.
    let claimed = runs.claim("default", "serve", Duration::from_secs(5)).await.unwrap();
    assert_eq!(claimed.map(|claim| claim.record.run_id), Some(served.run_id));
    assert!(runs.claim("default", "serve", Duration::from_secs(5)).await.unwrap().is_none());
}
