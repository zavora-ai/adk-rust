//! Two harness replicas over one shared backend (spec 3.1 §13 Q2): each replica has its own
//! scripted model, prompter, runner and store instances, and they share only the backend,
//! as two `adk-rust serve` processes do.

use std::sync::Arc;

use adk_core::audit_log::InMemoryAuditRecorder;
use adk_harness::{
    Harness, HarnessAgent, HarnessMode, ModeStore, PromptAnswer, RulePredicate, StandingRuleStore,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::SessionService;
use serde_json::json;

use crate::common::{Outcome, ScriptedPrompter, no_user_file, outcome, response_of};

/// The stores one replica holds, each its own instance over the shared backend.
pub struct ReplicaStores {
    pub modes: Arc<dyn ModeStore>,
    pub rules: Arc<dyn StandingRuleStore>,
    pub sessions: Arc<dyn SessionService>,
}

struct Replica {
    harness: Harness,
    prompter: Arc<ScriptedPrompter>,
}

fn bash(command: &str) -> ScriptedResponse {
    ScriptedResponse::function_call("bash", json!({ "command": command }))
}

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

fn replica(
    app: &str,
    workspace: &std::path::Path,
    stores: ReplicaStores,
    script: Vec<ScriptedResponse>,
) -> Replica {
    let prompter = ScriptedPrompter::new();
    let agent = HarnessAgent::builder("harness")
        .model(Arc::new(ScriptedLlm::new("scripted", script)))
        .workspace(adk_devtools::Workspace::new(workspace).allow_bash(true))
        .project_instructions(no_user_file())
        .without_compaction()
        .mode_store(stores.modes)
        .standing_rules(stores.rules)
        .prompter(prompter.clone())
        .build()
        .unwrap();
    let agent = Arc::new(agent);
    let runner = agent
        .configure_runner(
            Runner::builder().app_name(app).agent(agent.clone()).session_service(stores.sessions),
        )
        .audit_log(Arc::new(InMemoryAuditRecorder::new()))
        .build()
        .unwrap();
    Replica { harness: Harness::with_runner(agent, runner).unwrap(), prompter }
}

/// A rule granted through one replica is matched by the other; a revocation through the
/// first is refused by the second on its next call; a mode tightened on the first applies
/// on the second (spec 3.1 §9.2 I1, I4, I14).
pub async fn rules_and_modes_reach_every_replica(
    app: &str,
    first: ReplicaStores,
    second: ReplicaStores,
) {
    let workspace = tempfile::tempdir().unwrap();
    let a = replica(app, workspace.path(), first, vec![bash("echo from-a"), done()]);
    let b = replica(
        app,
        workspace.path(),
        second,
        vec![bash("echo from-b"), done(), bash("echo from-b"), done(), bash("echo from-b"), done()],
    );
    let user = crate::common::user("alice");
    let session =
        a.harness.open_session(&user, Some(&crate::common::session_id("s1"))).await.unwrap();
    assert_eq!(b.harness.open_session(&user, Some(&session)).await.unwrap(), session);

    // A person on replica A answers "always allow `echo`".
    a.prompter.then(|prompt| {
        let echo = prompt
            .suggestions
            .iter()
            .find(|suggestion| {
                suggestion.predicates
                    == [RulePredicate::ShellPrefix {
                        pointer: "/command".into(),
                        tokens: vec!["echo".into()],
                    }]
            })
            .expect("an `echo` prefix suggestion")
            .clone();
        PromptAnswer::ApproveAndRemember(echo)
    });
    let events =
        crate::common::collect(a.harness.send(&user, &session, "run").await.unwrap()).await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Executed);
    assert_eq!(a.prompter.count(), 1);
    let rules = a.harness.standing_rules().list(app, "alice").await.unwrap();
    assert_eq!(rules.len(), 1, "{rules:?}");

    // Replica B matches the rule A granted: no prompt.
    let events =
        crate::common::collect(b.harness.send(&user, &session, "run").await.unwrap()).await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Executed);
    assert_eq!(b.prompter.count(), 0, "the rule granted through replica A covers replica B");

    // A revokes; B's next call asks again, and B's prompter denies.
    assert!(a.harness.standing_rules().revoke(app, &rules[0].id).await.unwrap());
    let events =
        crate::common::collect(b.harness.send(&user, &session, "run").await.unwrap()).await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Denied);
    assert_eq!(b.prompter.count(), 1, "the revocation through replica A reached replica B");

    // A tightens the session to read-only; B denies bash without asking.
    a.harness.set_mode(&user, &session, HarnessMode::ReadOnly).await.unwrap();
    assert_eq!(b.harness.mode(&user, &session).await.unwrap(), HarnessMode::ReadOnly);
    let events =
        crate::common::collect(b.harness.send(&user, &session, "run").await.unwrap()).await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Denied);
    assert_eq!(b.prompter.count(), 1, "read-only denies without a prompt");
}
