//! Scripted end-to-end scenarios: a real `LlmAgent` behind a real `Runner`, a scripted
//! model, a temporary workspace, and assertions on files, stores and audit records
//! (spec 3.1 §11).

#![cfg(feature = "runner")]

use crate::common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::{
    ApprovalStore, DeclarativePolicy, PolicyDecision, Result, RunConfig, Tool, ToolContext,
    ToolPolicy, ToolPolicyRequest,
};
use adk_harness::{
    HarnessDecision, HarnessMode, InMemoryModeStore, InMemoryStandingRuleStore, ModeStore,
    PromptAnswer, RulePredicate, StandingRuleStore, TODOS_STATE_KEY, TrustDecision,
};
use adk_model::ScriptedResponse;
use common::{Outcome, fixture, fixture_in, outcome, response_of, responses};
use serde_json::{Value, json};

fn call(name: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call(name, args)
}

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

#[tokio::test]
async fn mode_matrix_decides_every_class_in_every_mode() {
    let expected = [
        (
            HarnessMode::ReadOnly,
            [Outcome::Executed, Outcome::Denied, Outcome::Denied, Outcome::Denied],
        ),
        (HarnessMode::Plan, [Outcome::Executed, Outcome::Denied, Outcome::Denied, Outcome::Held]),
        (HarnessMode::Ask, [Outcome::Executed, Outcome::Held, Outcome::Held, Outcome::Denied]),
        (HarnessMode::Auto, [Outcome::Executed, Outcome::Executed, Outcome::Held, Outcome::Denied]),
    ];
    for (mode, outcomes) in expected {
        let batch = ScriptedResponse::function_calls([
            ("read_file", json!({ "path": "README.md" })),
            ("write_file", json!({ "path": "out.txt", "content": "x" })),
            ("bash", json!({ "command": "touch ran.txt" })),
            ("exit_plan_mode", json!({ "plan": "do it" })),
        ]);
        let f = fixture(vec![batch, done()], |b| b.mode(mode)).await;
        std::fs::write(f.path("README.md"), "hello").unwrap();
        let events = f.send("go").await;
        let got: Vec<Outcome> = ["read_file", "write_file", "bash", "exit_plan_mode"]
            .iter()
            .map(|tool| outcome(&response_of(&events, tool)))
            .collect();
        assert_eq!(got, outcomes, "mode {mode}: {:#?}", responses(&events));
        assert_eq!(f.path("out.txt").exists(), outcomes[1] == Outcome::Executed, "mode {mode}");
        assert!(!f.path("ran.txt").exists(), "mode {mode}: host bash never runs unapproved");
    }
}

#[tokio::test]
async fn ask_holds_host_bash_until_a_decision_and_runs_it_once() {
    let f = fixture(
        vec![call("bash", json!({ "command": "echo once >> log.txt" })), done(), done()],
        |b| b,
    )
    .await;
    let events = f.send("go").await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Held);
    assert!(!f.path("log.txt").exists());
    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    assert_eq!(pending.len(), 1);
    f.harness
        .decide(&f.user, &f.session, &pending[0].fingerprint, HarnessDecision::Approve)
        .await
        .unwrap();
    f.resume().await;
    f.resume().await;
    assert_eq!(std::fs::read_to_string(f.path("log.txt")).unwrap(), "once\n");
    assert_eq!(f.records("harness.bash.begin").len(), 1);
    assert_eq!(f.records("harness.bash.end").len(), 1);
}

#[tokio::test]
async fn ask_allow_once_prompts_and_runs_once() {
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    let f = fixture(vec![call("bash", json!({ "command": "echo hi >> log.txt" })), done()], |b| {
        b.prompter(prompter.clone())
    })
    .await;
    let events = f.send("go").await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Executed);
    assert_eq!(std::fs::read_to_string(f.path("log.txt")).unwrap(), "hi\n");
    let prompts = prompter.prompts.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].rendered.title.contains("host"), "{}", prompts[0].rendered.title);
    assert_eq!(prompts[0].rendered.body, "$ echo hi >> log.txt");
    assert!(prompts[0].suggestions.is_empty(), "a redirection never gets a prefix rule");
}

#[tokio::test]
async fn always_allow_creates_a_standing_rule_for_this_user_only() {
    let prompter = common::ScriptedPrompter::new();
    prompter.then(|prompt| {
        let suggestion =
            prompt.suggestions.iter().find(|s| s.label.contains("cargo test")).unwrap().clone();
        PromptAnswer::ApproveAndRemember(suggestion)
    });
    prompter.deny();
    prompter.deny();
    let script = vec![
        call("bash", json!({ "command": "cargo test --quiet" })),
        done(),
        call("bash", json!({ "command": "cargo test --quiet --lib" })),
        done(),
        call("bash", json!({ "command": "cargo publish" })),
        done(),
        call("bash", json!({ "command": "cargo test --quiet" })),
        done(),
    ];
    let f = fixture(script, |b| b.prompter(prompter.clone())).await;
    f.send("test").await;
    assert_eq!(prompter.count(), 1);
    let events = f.send("test again").await;
    assert_eq!(prompter.count(), 1, "the rule covers cargo test --quiet --lib");
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Executed);
    let events = f.send("publish").await;
    assert_eq!(prompter.count(), 2, "cargo publish is not covered");
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Denied);

    let bob = common::user("bob");
    let bob_session = f.harness.open_session(&bob, None).await.unwrap();
    f.send_as(&bob, &bob_session, "test").await;
    assert_eq!(prompter.count(), 3, "alice's rule never applies to bob");

    let rules = f.harness.standing_rules().list(common::APP, "alice").await.unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(
        rules[0].predicates,
        vec![RulePredicate::ShellPrefix {
            pointer: "/command".into(),
            tokens: vec!["cargo".into(), "test".into()]
        }]
    );
    assert_eq!(f.records("harness.rule.created").len(), 1);
    assert_eq!(f.records("harness.bash.begin").len(), 2);
    // The policy itself applied the rule, through `ToolPolicy::evaluate_call` with the call's
    // origin and id, and recorded the match.
    let matched = f.records("harness.rule.matched");
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].ext["ruleId"], json!(rules[0].id));
    assert!(matched[0].ext["functionCallId"].is_string());
    assert_eq!(matched[0].policy_ref.as_deref(), Some("tool_policy:adk-harness"));
}

#[tokio::test]
async fn an_approver_can_edit_the_arguments_at_the_prompt() {
    let prompter = common::ScriptedPrompter::new();
    prompter
        .then(|_| PromptAnswer::ApproveWithArgs(json!({ "path": "edited.txt", "content": "y" })));
    prompter.then(|_| {
        PromptAnswer::ApproveWithArgs(json!({ "path": "../escape.txt", "content": "y" }))
    });
    let f = fixture(
        vec![
            call("write_file", json!({ "path": "notes.txt", "content": "x" })),
            done(),
            call("write_file", json!({ "path": "notes.txt", "content": "x" })),
            done(),
        ],
        |b| b.prompter(prompter.clone()),
    )
    .await;
    let events = f.send("write").await;
    assert_eq!(outcome(&response_of(&events, "write_file")), Outcome::Executed);
    assert_eq!(std::fs::read_to_string(f.path("edited.txt")).unwrap(), "y");
    assert!(!f.path("notes.txt").exists());
    let events = f.send("write again").await;
    let response = response_of(&events, "write_file");
    assert_eq!(outcome(&response), Outcome::Denied, "{response}");
    assert!(!f.dir.path().parent().unwrap().join("escape.txt").exists());
}

/// A tool whose description is set per build, so its declaration digest changes.
struct Deploy {
    description: &'static str,
    runs: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for Deploy {
    fn name(&self) -> &str {
        "deploy"
    }
    fn description(&self) -> &str {
        self.description
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(json!({ "deployed": true }))
    }
}

#[tokio::test]
async fn a_rule_goes_stale_when_the_tool_is_redeclared() {
    let rules: Arc<dyn StandingRuleStore> = Arc::new(InMemoryStandingRuleStore::new());
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let prompter = common::ScriptedPrompter::new();
    prompter.then(|prompt| PromptAnswer::ApproveAndRemember(prompt.suggestions[0].clone()));
    let v1 = {
        let (rules, runs, prompter) = (rules.clone(), runs.clone(), prompter.clone());
        fixture(
            vec![call("deploy", json!({})), done(), call("deploy", json!({})), done()],
            move |b| {
                b.standing_rules(rules)
                    .prompter(prompter)
                    .tool(Arc::new(Deploy { description: "Deploys v1", runs }))
            },
        )
        .await
    };
    v1.send("deploy").await;
    v1.send("deploy again").await;
    assert_eq!(prompter.count(), 1, "the second call is covered by the rule");
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 2);

    prompter.deny();
    let v2 = {
        let (rules, runs, prompter) = (rules.clone(), runs.clone(), prompter.clone());
        fixture(vec![call("deploy", json!({})), done()], move |b| {
            b.standing_rules(rules).prompter(prompter).tool(Arc::new(Deploy {
                description: "Deploys v2, now with database migrations",
                runs,
            }))
        })
        .await
    };
    v2.send("deploy").await;
    assert_eq!(prompter.count(), 2, "the redeclared tool needs a new approval");
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 2);
    let stale = v2.records("harness.rule.stale");
    assert_eq!(stale.len(), 1);
    assert_ne!(stale[0].ext["declarationDigest"], stale[0].ext["observedDigest"]);
}

#[tokio::test]
async fn edited_arguments_are_checked_by_the_policy_again() {
    let f = fixture(
        vec![call("write_file", json!({ "path": "notes.txt", "content": "x" })), done(), done()],
        |b| b,
    )
    .await;
    f.send("write").await;
    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    assert_eq!(pending.len(), 1);
    f.harness
        .decide(
            &f.user,
            &f.session,
            &pending[0].fingerprint,
            HarnessDecision::ApproveWithArgs(json!({ "path": "../escape.txt", "content": "x" })),
        )
        .await
        .unwrap();
    let events = f.resume().await;
    let response = response_of(&events, "write_file");
    assert_eq!(outcome(&response), Outcome::Denied, "{response}");
    assert!(response["error"].as_str().unwrap().contains("outside the workspace"));
    assert!(!f.dir.path().parent().unwrap().join("escape.txt").exists());
    assert!(!f.path("notes.txt").exists());
}

#[tokio::test]
async fn todos_are_isolated_per_session_and_reminded_when_they_change() {
    let script = vec![
        call(
            "write_todos",
            json!({ "todos": [{ "content": "Write the test", "status": "in_progress" }, { "content": "Fix the bug" }] }),
        ),
        done(),
        done(),
    ];
    let f = fixture(script, |b| b).await;
    f.send("plan it").await;
    let todos = f.harness.todos(&f.user, &f.session).await.unwrap();
    assert_eq!(todos.items.len(), 2);
    assert_eq!(todos.revision, 1);
    let other = f.harness.open_session(&f.user, None).await.unwrap();
    f.send_as(&f.user, &other, "hello").await;
    assert!(f.harness.todos(&f.user, &other).await.unwrap().items.is_empty());

    let requests = f.model.requests();
    let reminder = |index: usize| {
        requests[index]
            .contents
            .last()
            .and_then(|content| content.parts.first())
            .and_then(|part| match part {
                adk_core::Part::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default()
    };
    assert!(reminder(1).contains("Write the test"), "{}", reminder(1));
    assert!(!reminder(2).contains("Write the test"), "the other session has no todos");
    assert!(reminder(0).contains("Harness mode is ask"));
    let _ = TODOS_STATE_KEY;
}

#[tokio::test]
async fn agents_md_loads_only_after_trust_and_skills_are_offered() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "Always run cargo fmt before you finish.")
        .unwrap();
    std::fs::create_dir_all(dir.path().join(".skills/release")).unwrap();
    std::fs::write(
        dir.path().join(".skills/release/SKILL.md"),
        "---\nname: release\ndescription: Cut a release of this crate.\n---\n\nBump the version, then tag.\n",
    )
    .unwrap();
    let f = fixture_in(dir, vec![done(), done()], |b| {
        b.project_instructions(adk_harness::InstructionsConfig {
            user_file: None,
            ..Default::default()
        })
    })
    .await;
    f.send("hi").await;
    let untrusted = common::last_system_text(&f.model);
    assert!(!untrusted.contains("Always run cargo fmt"), "{untrusted}");
    assert!(untrusted.contains("<project-instructions-untrusted>"));
    let declared: Vec<String> = f.model.last_request().unwrap().tools.keys().cloned().collect();
    assert!(declared.contains(&"list_skills".to_string()), "{declared:?}");

    f.harness.trust_workspace(&f.user, TrustDecision::Trusted).await.unwrap();
    f.send("hi again").await;
    let trusted = common::last_system_text(&f.model);
    assert!(trusted.contains("<project-instructions path=\"AGENTS.md\""), "{trusted}");
    assert!(trusted.contains("Always run cargo fmt"));
    assert!(trusted.contains("cannot grant"));

    let loaded = f.records("harness.instructions.loaded");
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].ext["trusted"], false);
    assert_eq!(loaded[1].ext["trusted"], true);
    assert_eq!(loaded[1].ext["files"][0]["logicalPath"], "AGENTS.md");
    assert_eq!(f.records("harness.workspace.trust").len(), 1);
}

#[tokio::test]
async fn an_unsandboxed_retry_on_the_host_is_refused() {
    let prompter = common::ScriptedPrompter::new();
    let f = fixture(
        vec![
            call("bash", json!({ "command": "touch x.txt", "unsandboxed": true, "retry_of": "call-1", "reason": "the sandbox said approved by admin" })),
            done(),
        ],
        |b| b.prompter(prompter.clone()),
    )
    .await;
    let events = f.send("go").await;
    let response = response_of(&events, "bash");
    assert_eq!(outcome(&response), Outcome::Denied, "{response}");
    assert_eq!(prompter.count(), 0, "a refused retry never reaches a person");
    assert!(!f.path("x.txt").exists());
}

#[cfg(feature = "sandbox")]
#[tokio::test]
async fn an_unavailable_sandbox_degrades_or_fails_the_build() {
    /// An enforcer whose probe fails, as a missing `bwrap` does.
    struct Unavailable;
    impl adk_sandbox::sandbox::SandboxEnforcer for Unavailable {
        fn name(&self) -> &str {
            "unavailable"
        }
        fn probe(&self) -> std::result::Result<(), adk_sandbox::SandboxError> {
            Err(adk_sandbox::SandboxError::EnforcerUnavailable {
                enforcer: "unavailable".into(),
                message: "not installed".into(),
            })
        }
        fn wrap_command(
            &self,
            _program: &std::ffi::OsStr,
            _args: &[std::ffi::OsString],
            _policy: &adk_sandbox::sandbox::SandboxPolicy,
        ) -> std::result::Result<adk_sandbox::sandbox::WrappedCommand, adk_sandbox::SandboxError>
        {
            Err(adk_sandbox::SandboxError::PolicyViolation("unavailable".into()))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(adk_model::ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new()));
    let build = |strict: bool| {
        let mut builder = adk_harness::HarnessAgent::builder("harness")
            .model(model.clone())
            .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
            .project_instructions(common::no_user_file())
            .shell(adk_harness::ShellIsolation::OsSandbox(adk_harness::OsSandboxConfig {
                enforcer: Some(Arc::new(Unavailable)),
                ..adk_harness::OsSandboxConfig::default()
            }));
        if strict {
            builder = builder.fail_if_unavailable();
        }
        builder.build()
    };
    let degraded = build(false).unwrap();
    assert!(matches!(degraded.info().isolation, adk_harness::IsolationStatus::Degraded { .. }));
    assert!(degraded.info().degraded_reason.is_some());
    let error = build(true).unwrap_err();
    assert_eq!(error.code, adk_harness::SANDBOX_UNAVAILABLE_CODE);
}

#[tokio::test]
async fn an_approved_plan_loosens_the_mode_from_the_next_invocation() {
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    prompter.approve_once();
    let script = vec![
        ScriptedResponse::function_calls([
            (
                "exit_plan_mode",
                json!({ "plan": "1. write the file", "steps": [{ "id": "1", "title": "write", "instruction": "write it" }] }),
            ),
            ("write_file", json!({ "path": "plan.txt", "content": "x" })),
        ]),
        call("write_file", json!({ "path": "plan.txt", "content": "x" })),
        done(),
    ];
    let f = fixture(script, |b| b.mode(HarnessMode::Plan).prompter(prompter.clone())).await;
    let events = f.send("plan").await;
    assert_eq!(outcome(&response_of(&events, "exit_plan_mode")), Outcome::Executed);
    assert_eq!(
        outcome(&response_of(&events, "write_file")),
        Outcome::Denied,
        "same invocation stays in plan"
    );
    assert!(!f.path("plan.txt").exists());
    assert_eq!(f.model.requests().len(), 1, "an approved plan ends the turn");
    assert_eq!(f.harness.mode(&f.user, &f.session).await.unwrap(), HarnessMode::Ask);
    let mirror = events
        .iter()
        .find_map(|event| event.actions.state_delta.get("harness:mode").cloned())
        .unwrap();
    assert_eq!(mirror["mode"], "ask");

    f.send("go").await;
    assert!(f.path("plan.txt").exists(), "ask mode runs the approved write");
    assert_eq!(prompter.count(), 2);
    assert_eq!(f.records("harness.mode.changed").len(), 1);
}

#[tokio::test]
async fn loosening_the_mode_cancels_held_calls() {
    let f =
        fixture(vec![call("bash", json!({ "command": "touch held.txt" })), done(), done()], |b| b)
            .await;
    f.send("go").await;
    assert_eq!(f.harness.pending_approvals(&f.user, &f.session).await.unwrap().len(), 1);
    f.harness.set_mode(&f.user, &f.session, HarnessMode::Auto).await.unwrap();
    f.resume().await;
    assert!(
        !f.path("held.txt").exists(),
        "a held call never runs under a mode its approver did not see"
    );
    let cancelled = f.records("harness.held.cancelled");
    assert_eq!(cancelled.len(), 1);
    let changed = f.records("harness.mode.changed");
    assert_eq!(changed[0].ext["cancelledHeldCallIds"].as_array().unwrap().len(), 1);
    assert_eq!(f.harness.mode(&f.user, &f.session).await.unwrap(), HarnessMode::Auto);
}

#[tokio::test]
async fn set_mode_tightens_and_loosens_with_revisions() {
    let store: Arc<dyn ModeStore> = Arc::new(InMemoryModeStore::new());
    let f = {
        let store = store.clone();
        fixture(vec![done()], move |b| b.mode_store(store).mode(HarnessMode::Plan)).await
    };
    f.harness.set_mode(&f.user, &f.session, HarnessMode::ReadOnly).await.unwrap();
    assert_eq!(f.harness.mode(&f.user, &f.session).await.unwrap(), HarnessMode::ReadOnly);
    f.harness.set_mode(&f.user, &f.session, HarnessMode::Ask).await.unwrap();
    assert_eq!(store.get(common::APP, "alice", "s1").await.unwrap().unwrap().revision, 2);
}

#[tokio::test]
async fn configure_run_composes_with_the_runs_policy_and_is_idempotent() {
    let f = fixture(vec![], |b| b).await;
    let agent = f.harness.agent();
    let operator: Arc<dyn ToolPolicy> = Arc::new(
        DeclarativePolicy::builder().deny("read_file", "not here").allow("*").name("ops").build(),
    );
    let mut config = RunConfig { tool_policy: Some(operator), ..RunConfig::default() };
    for _ in 0..3 {
        adk_core::Agent::configure_run(agent.as_ref(), "harness", &mut config);
    }
    let policy = config.tool_policy.clone().unwrap();
    assert_eq!(policy.policy_ref().as_deref(), Some("ops+adk-harness"));
    let request = ToolPolicyRequest::new("read_file", json!({ "path": "a" }))
        .with_agent_name("harness")
        .with_identity(common::APP, "alice", "s1");
    assert!(
        matches!(policy.evaluate(&request).await, PolicyDecision::Deny { .. }),
        "outer deny wins"
    );

    let own_store: Arc<dyn ApprovalStore> = Arc::new(adk_core::InMemoryApprovalStore::new());
    let mut config = RunConfig { approval_store: Some(own_store.clone()), ..RunConfig::default() };
    adk_core::Agent::configure_run(agent.as_ref(), "harness", &mut config);
    assert!(
        Arc::ptr_eq(config.approval_store.as_ref().unwrap(), &own_store),
        "the run's own store is kept"
    );
    let mut config = RunConfig::default();
    adk_core::Agent::configure_run(agent.as_ref(), "harness", &mut config);
    assert!(Arc::ptr_eq(config.approval_store.as_ref().unwrap(), agent.runtime().approval_store()));

    let held = adk_core::ToolConfirmationRequest::new("exit_plan_mode", json!({ "plan": "x" }));
    let mut config = RunConfig::default();
    config.tool_approvals.insert(held.fingerprint(), adk_core::ToolApproval::approve());
    adk_core::Agent::configure_run(agent.as_ref(), "harness", &mut config);
    assert!(config.tool_approvals.is_empty(), "non-waivable approvals are stripped");
}

#[tokio::test]
async fn a_nested_harness_refuses_to_run_ungoverned() {
    let f = fixture(vec![done()], |b| b).await;
    let nested = adk_agent::SequentialAgent::new(
        "outer",
        vec![f.harness.agent().clone() as Arc<dyn adk_core::Agent>],
    );
    let sessions: Arc<dyn adk_session::SessionService> =
        Arc::new(adk_session::InMemorySessionService::new());
    let runner = adk_runner::Runner::builder()
        .app_name("nested")
        .agent(Arc::new(nested))
        .session_service(sessions.clone())
        .build()
        .unwrap();
    sessions
        .create(adk_session::CreateRequest {
            app_name: "nested".into(),
            user_id: "alice".into(),
            session_id: Some("s".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let stream =
        runner.run_str("alice", "s", adk_core::Content::new("user").with_text("hi")).await.unwrap();
    let results: Vec<_> = futures::StreamExt::collect(stream).await;
    let error =
        results.into_iter().find_map(|result| result.err()).expect("the nested harness refuses");
    assert_eq!(error.code, adk_harness::HARNESS_UNGOVERNED_CODE);
    assert_eq!(f.model.requests().len(), 0);
}

#[tokio::test]
async fn a_freeze_kills_a_running_command() {
    let f = fixture(
        vec![call("bash", json!({ "command": "sleep 20; touch late.txt" })), done()],
        |b| b.mode(HarnessMode::Auto).allow_host_shell_in_auto(),
    )
    .await;
    let governance = f.harness.runner().governance().clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        governance.freeze("incident");
    });
    let started = Instant::now();
    let stream = f.harness.send(&f.user, &f.session, "go").await.unwrap();
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        futures::StreamExt::collect::<Vec<_>>(stream),
    )
    .await
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
    let end = f.records("harness.bash.end");
    assert_eq!(end.len(), 1);
    assert_eq!(end[0].ext["killedByFreeze"], true);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!f.path("late.txt").exists());
}

#[tokio::test]
async fn a_denial_ends_the_turns_prompting() {
    let prompter = common::ScriptedPrompter::new();
    prompter.deny();
    let f = fixture(
        vec![
            ScriptedResponse::function_calls([
                ("bash", json!({ "command": "touch a.txt" })),
                ("bash", json!({ "command": "touch b.txt" })),
            ]),
            done(),
        ],
        |b| b.prompter(prompter.clone()),
    )
    .await;
    let events = f.send("go").await;
    assert_eq!(prompter.count(), 1, "the second call is denied without a prompt");
    assert!(
        responses(&events)
            .iter()
            .filter(|(name, _)| name == "bash")
            .all(|(_, r)| outcome(r) == Outcome::Denied)
    );
    assert!(!f.path("a.txt").exists() && !f.path("b.txt").exists());
}

#[tokio::test]
async fn a_truncated_call_cannot_be_approved_unexpanded() {
    let long = format!("echo {} > big.txt", "x".repeat(6000));
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    prompter.then(|prompt| {
        let _ = prompt.rendered.expand();
        PromptAnswer::ApproveOnce
    });
    let f = fixture(
        vec![
            call("bash", json!({ "command": long.clone() })),
            done(),
            call("bash", json!({ "command": long })),
            done(),
        ],
        |b| b.prompter(prompter.clone()),
    )
    .await;
    let events = f.send("go").await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Denied);
    assert!(!f.path("big.txt").exists());
    let events = f.send("again").await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Executed);
    assert!(f.path("big.txt").exists());
}

#[tokio::test]
async fn bash_refuses_to_run_outside_the_harness_policy() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Arc::new(adk_harness::InMemoryShellLedger::new());
    let bash = adk_harness::HarnessBashTool::new(
        adk_devtools::Workspace::new(dir.path()).allow_bash(true),
        adk_harness::IsolationStatus::Host,
        false,
        ledger,
    );
    let model = Arc::new(adk_model::ScriptedLlm::new(
        "scripted",
        vec![call("bash", json!({ "command": "touch plain.txt" })), done()],
    ));
    let agent =
        adk_agent::LlmAgentBuilder::new("plain").model(model).tool(Arc::new(bash)).build().unwrap();
    let sessions: Arc<dyn adk_session::SessionService> =
        Arc::new(adk_session::InMemorySessionService::new());
    sessions
        .create(adk_session::CreateRequest {
            app_name: "plain".into(),
            user_id: "alice".into(),
            session_id: Some("s".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let runner = adk_runner::Runner::builder()
        .app_name("plain")
        .agent(Arc::new(agent))
        .session_service(sessions)
        .build()
        .unwrap();
    let stream =
        runner.run_str("alice", "s", adk_core::Content::new("user").with_text("go")).await.unwrap();
    let events = common::collect(stream).await;
    let response = response_of(&events, "bash");
    assert!(
        response["error"].as_str().unwrap_or_default().contains("harness.bash.ungoverned"),
        "{response}"
    );
    assert!(!dir.path().join("plain.txt").exists());
}
