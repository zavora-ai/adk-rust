//! Team lowering and members (spec 3.1 §7.10, §9.2 I35, §11).

#![cfg(all(feature = "team", feature = "runner"))]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use adk_agent::{
    CircuitBreakerPolicy, RelationshipApprovalPolicy, RelationshipFailureStrategy,
    RelationshipKind, RelationshipPolicy, StaticTeamAgentRegistry, TeamAgentDescriptor,
    TeamAgentHealth, TeamBudget, TeamContextPolicy, TeamHistoryPolicy, TeamMemberSpec, TeamPolicy,
    TeamRelationship, TeamResumePolicy, TeamStateMergePolicy, TeamTerminationPolicy,
};
use adk_core::{Agent, Content, Event, EventStream, InvocationContext, Part};
use adk_harness::{HarnessMode, MemberKind, MemberSpec, MemberTools};
use adk_model::{ScriptedLlm, ScriptedResponse};
use common::*;
use serde_json::json;

fn call(name: &str, args: serde_json::Value) -> ScriptedResponse {
    ScriptedResponse::function_call(name, args)
}

fn text(text: &str) -> ScriptedResponse {
    ScriptedResponse::text(text)
}

fn expected_policy(timeout_ms: u64, failure: RelationshipFailureStrategy) -> RelationshipPolicy {
    RelationshipPolicy {
        input_schema: None,
        output_schema: None,
        context: Some(TeamContextPolicy::Isolated),
        history: TeamHistoryPolicy::None,
        state_keys: Vec::new(),
        state_write_keys: Vec::new(),
        state_merge: TeamStateMergePolicy::RejectConflicts,
        artifact_prefixes: Vec::new(),
        resume: TeamResumePolicy::FailClosed,
        timeout_ms: Some(timeout_ms),
        approval: RelationshipApprovalPolicy::Never,
        failure,
        circuit_breaker: Some(CircuitBreakerPolicy {
            failure_threshold: 3,
            reset_after_ms: 60_000,
        }),
    }
}

#[tokio::test]
async fn member_lowering_sets_every_policy_field() {
    let mut fast = MemberSpec::general();
    fast.name = "fixer".into();
    fast.timeout = Duration::from_secs(90);
    let f = fixture(vec![text("done")], move |b| b.member(fast)).await;
    let team = f.harness.agent().runtime().team().expect("default and added members");
    let spec = team.spec();
    assert_eq!(spec.name, "harness_team");
    assert_eq!(spec.coordinator, "harness");
    assert_eq!(
        spec.members.iter().map(|member| member.name.as_str()).collect::<Vec<_>>(),
        vec!["harness", "explore", "general", "fixer"]
    );
    assert_eq!(
        spec.relationships,
        vec![
            TeamRelationship::new("harness", "explore", RelationshipKind::Delegate).with_policy(
                expected_policy(
                    900_000,
                    RelationshipFailureStrategy::Retry { max_attempts: 2, backoff_ms: 1_000 }
                )
            ),
            TeamRelationship::new("harness", "general", RelationshipKind::Delegate).with_policy(
                expected_policy(
                    900_000,
                    RelationshipFailureStrategy::Retry { max_attempts: 1, backoff_ms: 0 }
                )
            ),
            TeamRelationship::new("harness", "fixer", RelationshipKind::Delegate).with_policy(
                expected_policy(
                    90_000,
                    RelationshipFailureStrategy::Retry { max_attempts: 1, backoff_ms: 0 }
                )
            ),
        ]
    );
    assert_eq!(
        spec.policy,
        TeamPolicy {
            max_transfer_depth: 8,
            max_delegation_depth: 1,
            max_concurrent_delegations: 4,
            context: TeamContextPolicy::Isolated,
            failure: adk_agent::TeamFailurePolicy::Propagate,
            budget: TeamBudget::default(),
            termination: TeamTerminationPolicy::default(),
        }
    );
    assert_eq!(f.harness.agent().info().members, vec!["explore", "fixer", "general"]);
    // The harness keeps its name with members, so its identity does not change.
    assert_eq!(f.harness.runner().root_agent_name(), "harness");
}

#[tokio::test]
async fn a_caller_team_policy_keeps_its_budget_but_not_its_context_or_depth() {
    let policy = TeamPolicy {
        context: TeamContextPolicy::Shared,
        max_delegation_depth: 5,
        budget: TeamBudget { max_delegations: Some(1), ..TeamBudget::default() },
        ..TeamPolicy::default()
    };
    let f = fixture(vec![text("done")], move |b| b.team_policy(policy)).await;
    let spec = f.harness.agent().runtime().team().unwrap().spec().clone();
    assert_eq!(spec.policy.context, TeamContextPolicy::Isolated);
    assert_eq!(spec.policy.max_delegation_depth, 1);
    assert_eq!(spec.policy.budget.max_delegations, Some(1));
}

#[tokio::test]
async fn without_default_members_and_none_added_there_is_no_team() {
    let f = fixture(vec![text("done")], |b| b.without_default_members()).await;
    assert!(f.harness.agent().runtime().team().is_none());
    assert!(f.harness.agent().info().members.is_empty());
}

/// Builds a fixture whose members answer from `member_script`.
async fn team_fixture(
    script: Vec<ScriptedResponse>,
    member_script: Vec<ScriptedResponse>,
    mode: HarnessMode,
) -> (Fixture, Arc<ScriptedLlm>) {
    let members = Arc::new(ScriptedLlm::new("member", member_script));
    let member_model = members.clone();
    let f = fixture(script, move |b| b.mode(mode).member_model(member_model)).await;
    (f, members)
}

#[tokio::test]
async fn a_member_inherits_the_parents_stricter_mode() {
    // spec 3.1 §11: "member inherits the stricter mode". In Plan, the general member has
    // write_file and bash, and both are refused.
    let (f, members) = team_fixture(
        vec![call("general", json!({ "request": "write notes.txt and run ls" })), text("ok")],
        vec![
            call("write_file", json!({ "path": "notes.txt", "content": "x" })),
            call("bash", json!({ "command": "touch ran.txt" })),
            text("could not"),
        ],
        HarnessMode::Plan,
    )
    .await;
    let events = f.send("delegate it").await;
    let write = response_of(&events, "write_file");
    assert_eq!(outcome(&write), Outcome::Denied, "{write}");
    assert!(write["error"].as_str().unwrap().contains("plan mode"), "{write}");
    let bash = response_of(&events, "bash");
    assert_eq!(outcome(&bash), Outcome::Denied, "{bash}");
    assert!(!f.path("notes.txt").exists());
    assert!(!f.path("ran.txt").exists());
    assert_eq!(members.remaining(), 0);

    // Tightening applies at the next evaluation for members too: Ask holds the write for a
    // decision instead of refusing it.
    f.harness.set_mode(&f.user, &f.session, HarnessMode::Ask).await.unwrap();
    members.push(call("write_file", json!({ "path": "notes.txt", "content": "x" })));
    members.push(text("waiting"));
    f.model.push(call("general", json!({ "request": "write notes.txt" })));
    f.model.push(text("held"));
    let events = f.send("again").await;
    let delegated = response_of(&events, "general");
    assert_eq!(outcome(&delegated), Outcome::Held, "{delegated}");
    assert!(!f.path("notes.txt").exists());
}

#[tokio::test]
async fn a_delegation_holding_two_calls_is_answered_once_when_the_mode_loosens() {
    // Ask holds both member writes under the one `general` call (spec 3.1 §10 F4), and the
    // loosening that cancels them answers that call exactly once (spec 3.1 §7.4).
    let (f, _members) = team_fixture(
        vec![call("general", json!({ "request": "write a.txt and b.txt" })), text("ok")],
        vec![
            ScriptedResponse::function_calls([
                ("write_file", json!({ "path": "a.txt", "content": "a" })),
                ("write_file", json!({ "path": "b.txt", "content": "b" })),
            ]),
            text("waiting"),
        ],
        HarnessMode::Ask,
    )
    .await;
    let events = f.send("delegate it").await;
    let delegated = response_of(&events, "general");
    assert_eq!(outcome(&delegated), Outcome::Held, "{delegated}");
    assert_eq!(
        f.harness.pending_approvals(&f.user, &f.session).await.unwrap().len(),
        2,
        "both member calls wait under the one delegation"
    );

    f.harness.set_mode(&f.user, &f.session, HarnessMode::Auto).await.unwrap();
    let cancelled = f.records("harness.held.cancelled");
    assert_eq!(cancelled.len(), 1);
    let ids = cancelled[0].ext["callIds"].as_array().unwrap().clone();
    assert_eq!(ids.len(), 1, "the delegation's call id, once: {ids:?}");
    let changed = f.records("harness.mode.changed");
    assert_eq!(changed[0].ext["cancelledHeldCallIds"], json!(ids));

    // The next run's history answers the delegation once.
    f.resume().await;
    let request = f.model.last_request().unwrap();
    let answers = request
        .contents
        .iter()
        .flat_map(|content| content.parts.iter())
        .filter(|part| {
            matches!(part, Part::FunctionResponse { function_response, .. } if function_response.name == "general")
        })
        .count();
    assert_eq!(answers, 1, "{request:?}");
    assert!(!f.path("a.txt").exists() && !f.path("b.txt").exists());
}

#[tokio::test]
async fn members_run_isolated_and_their_state_never_reaches_the_parent() {
    let (f, members) = team_fixture(
        vec![call("explore", json!({ "request": "find the entry point" })), text("found")],
        vec![
            call("write_todos", json!({ "todos": [{ "content": "Look at main.rs" }] })),
            call("update_todo", json!({ "id": "1", "status": "completed" })),
            text("src/main.rs:1"),
        ],
        HarnessMode::Ask,
    )
    .await;
    let events = f.send("where does it start? SECRET-PARENT-CONTEXT").await;
    // The member's own todos work inside the member...
    let update = response_of(&events, "update_todo");
    assert_eq!(outcome(&update), Outcome::Executed, "{update}");
    // ...and never merge back into the parent session.
    assert!(f.harness.todos(&f.user, &f.session).await.unwrap().items.is_empty());
    // The member saw only the delegated request, not the parent's conversation.
    let first = &members.requests()[0];
    let seen = serde_json::to_string(&first.contents).unwrap();
    assert!(seen.contains("find the entry point"), "{seen}");
    assert!(!seen.contains("SECRET-PARENT-CONTEXT"), "{seen}");
    let answer = response_of(&events, "explore");
    assert!(answer.to_string().contains("src/main.rs:1"), "{answer}");
}

#[tokio::test]
async fn delegations_are_audited_with_the_member_and_its_parent() {
    let (f, _members) = team_fixture(
        vec![call("explore", json!({ "request": "look" })), text("ok")],
        vec![text("nothing to report")],
        HarnessMode::Ask,
    )
    .await;
    f.send("look around").await;
    let spawned = f.records("harness.member.spawned");
    assert_eq!(spawned.len(), 1, "{spawned:?}");
    let ext = &spawned[0].ext;
    assert_eq!(ext["member"], "explore");
    assert_eq!(ext["kind"], "local");
    assert_eq!(ext["depth"], 1);
    assert_eq!(ext["timeoutMs"], 900_000);
    assert!(ext["memberAgentId"].as_str().unwrap().ends_with("/explore"), "{ext:?}");
    assert!(ext["parentAgentId"].as_str().unwrap().ends_with("/harness"), "{ext:?}");
    let finished = f.records("harness.member.finished");
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].ext["outcome"], "completed");
}

#[tokio::test]
async fn a_delegation_runs_in_a_harness_member_span() {
    let log = SpanLog::default();
    let _guard = log.install();
    let (f, _members) = team_fixture(
        vec![call("explore", json!({ "request": "look" })), text("ok")],
        vec![text("nothing")],
        HarnessMode::Ask,
    )
    .await;
    f.send("look around").await;
    let lines = log.lines();
    assert!(
        lines.iter().any(|line| line.starts_with("harness.member harness.member=\"explore\"")),
        "{lines:#?}"
    );
}

#[cfg(feature = "observe")]
#[tokio::test]
async fn a_member_carries_the_coordinators_loop_guard() {
    // spec 3.1 §7.13 on members: the coordinator toolset's observers run for member requests.
    let read = || call("read_file", json!({ "path": "notes.txt" }));
    let members =
        Arc::new(ScriptedLlm::new("member", vec![read(), read(), read(), read(), text("x")]));
    let member_model = members.clone();
    let f = fixture(
        vec![call("explore", json!({ "request": "read the notes" })), text("ok")],
        move |b| b.member_model(member_model).loop_guard(adk_harness::LoopGuard::default()),
    )
    .await;
    std::fs::write(f.path("notes.txt"), "hello").unwrap();
    let stream = f.harness.send(&f.user, &f.session, "look").await.unwrap();
    let _: Vec<_> = futures::StreamExt::collect(stream).await;
    assert_eq!(f.records("harness.loop.detected").len(), 1, "the member's loop was detected");
    let record = f
        .harness
        .agent()
        .runtime()
        .mode_store()
        .get(APP, "alice", "s1")
        .await
        .unwrap()
        .expect("the guard tightened the shared session");
    assert_eq!(
        (record.mode, record.set_by),
        (HarnessMode::ReadOnly, adk_harness::ModeSource::SystemTighten)
    );
    let _ = members;
}

#[tokio::test]
async fn members_cannot_delegate_further() {
    // spec 3.1 §9.2 I35: delegation depth 1. A member offered no delegate tools.
    let (f, members) = team_fixture(
        vec![call("general", json!({ "request": "do it" })), text("ok")],
        vec![text("done")],
        HarnessMode::Ask,
    )
    .await;
    f.send("go").await;
    let tools: Vec<String> = members.requests()[0].tools.keys().cloned().collect();
    assert!(!tools.iter().any(|tool| tool == "explore" || tool == "general"), "{tools:?}");
    assert!(!tools.iter().any(|tool| tool == "exit_plan_mode"), "{tools:?}");
    assert!(tools.iter().any(|tool| tool == "write_file"), "{tools:?}");
}

#[tokio::test]
async fn the_team_budget_stops_delegations_at_its_cap() {
    let members = Arc::new(ScriptedLlm::new("member", vec![text("one"), text("two")]));
    let member_model = members.clone();
    let f = fixture(
        vec![
            call("explore", json!({ "request": "first" })),
            call("explore", json!({ "request": "second" })),
            text("done"),
        ],
        move |b| {
            b.member_model(member_model).team_policy(TeamPolicy {
                budget: TeamBudget { max_delegations: Some(1), ..TeamBudget::default() },
                ..TeamPolicy::default()
            })
        },
    )
    .await;
    let stream = f.harness.send(&f.user, &f.session, "twice").await.unwrap();
    let results: Vec<_> = futures::StreamExt::collect(stream).await;
    let failed = results
        .iter()
        .any(|result| result.as_ref().err().is_some_and(|error| error.code.contains("budget")))
        || results.iter().filter_map(|r| r.as_ref().ok()).any(|event| {
            responses(std::slice::from_ref(event)).iter().any(|(name, response)| {
                name == "explore" && response.to_string().contains("budget")
            })
        });
    assert!(failed, "the second delegation must stop at the team budget: {results:#?}");
    assert_eq!(members.remaining(), 1, "only one delegation ran");
}

/// A caller-supplied agent the registry offers.
struct Docs;

#[async_trait::async_trait]
impl Agent for Docs {
    fn name(&self) -> &str {
        "docs"
    }

    fn description(&self) -> &str {
        "Answers documentation questions."
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        let mut event = Event::new(ctx.invocation_id());
        event.author = "docs".into();
        event.llm_response.content = Some(Content::new("model").with_text("see docs/intro.md"));
        Ok(Box::pin(futures::stream::iter(vec![Ok(event)])))
    }
}

#[tokio::test]
async fn members_are_discovered_in_a_registry_and_delegated_to() {
    let registry = StaticTeamAgentRegistry::new()
        .register(
            TeamAgentDescriptor {
                binding: "docs-v1".into(),
                name: "docs".into(),
                description: "Answers documentation questions.".into(),
                capabilities: Vec::new(),
                priority: 0,
                version: None,
                digest: None,
                trust_labels: Vec::new(),
                health: TeamAgentHealth::Healthy,
                expires_at_ms: None,
            },
            Arc::new(Docs),
        )
        .unwrap();
    let f = fixture(
        vec![call("docs", json!({ "request": "where are the docs?" })), text("ok")],
        move |b| b.without_default_members().member_registry(Arc::new(registry)),
    )
    .await;
    assert_eq!(f.harness.agent().info().members, vec!["docs"]);
    assert_eq!(
        f.harness.agent().runtime().team().unwrap().spec().members[1],
        TeamMemberSpec::new("docs").with_description("Answers documentation questions.")
    );
    let events = f.send("docs?").await;
    let answer = response_of(&events, "docs");
    assert!(answer.to_string().contains("docs/intro.md"), "{answer}");
}

#[tokio::test]
async fn an_agent_member_is_wrapped_and_its_tools_are_external() {
    let member = MemberSpec {
        name: "docs".into(),
        description: "Answers documentation questions.".into(),
        kind: MemberKind::Agent(Arc::new(Docs)),
        tools: MemberTools::ReadOnly,
        skills: false,
        max_iterations: 50,
        timeout: Duration::from_secs(60),
        output_schema: None,
    };
    let f = fixture(vec![call("docs", json!({ "request": "where?" })), text("ok")], move |b| {
        b.without_default_members().member(member)
    })
    .await;
    let events = f.send("docs?").await;
    assert!(response_of(&events, "docs").to_string().contains("docs/intro.md"));
    let registry = f.harness.agent().runtime().policy().registry();
    let kind = registry.classify(
        "docs",
        "read_file",
        None,
        &json!({}),
        adk_core::ToolEffect::NonIdempotent,
        adk_harness::Isolation::Host,
        false,
    );
    assert!(
        matches!(kind, adk_harness::ToolKind::External { .. }),
        "a caller-supplied agent is not one of the harness's own agents: {kind:?}"
    );
    assert_eq!(
        registry.classify(
            "harness",
            "docs",
            None,
            &json!({}),
            adk_core::ToolEffect::NonIdempotent,
            adk_harness::Isolation::Host,
            false,
        ),
        adk_harness::ToolKind::Delegate
    );
}

#[cfg(all(unix, feature = "team-acp"))]
mod acp {
    use super::*;

    fn acp_member(target: &str) -> MemberSpec {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/scripted_acp_agent.py");
        MemberSpec::reviewer_acp(&format!("python3 {script} {target}"))
    }

    async fn acp_fixture(mode: HarnessMode, prompter: bool) -> (Fixture, Arc<ScriptedPrompter>) {
        let answers = ScriptedPrompter::new();
        let handler = answers.clone();
        let f = fixture(
            vec![call("reviewer", json!({ "request": "review the change" })), text("ok")],
            move |b| {
                let b = b.mode(mode).without_default_members().member(acp_member("REVIEW.md"));
                if prompter { b.prompter(handler) } else { b }
            },
        )
        .await;
        (f, answers)
    }

    #[tokio::test]
    async fn an_acp_member_in_plan_may_read_but_not_edit() {
        let (f, _) = acp_fixture(HarnessMode::Plan, false).await;
        let events = f.send("review").await;
        let answer = response_of(&events, "reviewer").to_string();
        assert!(answer.contains("read: allowed"), "{answer}");
        assert!(answer.contains("edit: denied"), "{answer}");
        assert!(!f.path("REVIEW.md").exists());
    }

    #[tokio::test]
    async fn an_acp_edit_in_ask_is_put_to_the_person_and_runs_once() {
        let (f, prompter) = acp_fixture(HarnessMode::Ask, true).await;
        prompter.approve_once();
        let events = f.send("review").await;
        let answer = response_of(&events, "reviewer").to_string();
        assert!(answer.contains("edit: written"), "{answer}");
        assert_eq!(prompter.count(), 1, "only the edit needs a person; the read is allowed");
        assert_eq!(std::fs::read_to_string(f.path("acp-edits.log")).unwrap(), "REVIEW.md\n");
        let spawned = f.records("harness.member.spawned");
        assert_eq!(spawned[0].ext["kind"], "acp");
    }

    #[tokio::test]
    async fn an_acp_edit_no_one_can_approve_live_is_refused() {
        let (f, _) = acp_fixture(HarnessMode::Ask, false).await;
        let events = f.send("review").await;
        let answer = response_of(&events, "reviewer").to_string();
        assert!(answer.contains("edit: denied"), "{answer}");
        assert!(!f.path("REVIEW.md").exists());
        assert!(
            f.harness.pending_approvals(&f.user, &f.session).await.unwrap().is_empty(),
            "an ACP session cannot wait, so nothing is left in the approvals inbox"
        );
    }

    #[tokio::test]
    async fn an_acp_edit_of_a_protected_path_is_refused_in_auto() {
        let answers = ScriptedPrompter::new();
        let handler = answers.clone();
        let f =
            fixture(vec![call("reviewer", json!({ "request": "review" })), text("ok")], move |b| {
                b.mode(HarnessMode::Auto)
                    .without_default_members()
                    .member(acp_member("AGENTS.md"))
                    .prompter(handler)
            })
            .await;
        answers.deny();
        let events = f.send("review").await;
        let answer = response_of(&events, "reviewer").to_string();
        assert!(answer.contains("edit: denied"), "{answer}");
        assert_eq!(answers.count(), 1, "a protected path needs a one-time human decision");
        assert!(!f.path("AGENTS.md").exists());
    }
}

#[cfg(feature = "team-a2a")]
#[tokio::test]
async fn a_remote_member_runs_as_an_audited_a2a_delegation() {
    let member = MemberSpec {
        name: "remote".into(),
        description: "A remote agent over A2A.".into(),
        kind: MemberKind::Remote(adk_server::a2a::RemoteA2aConfig {
            name: "ignored".into(),
            description: String::new(),
            // Nothing listens here, so the member reports the failure as its answer.
            agent_url: "http://127.0.0.1:9".into(),
            streaming: Some(false),
            credential_broker: None,
            credential_scopes: Vec::new(),
            allowed_endpoint_origins: Vec::new(),
        }),
        tools: MemberTools::ReadOnly,
        skills: false,
        max_iterations: 50,
        timeout: Duration::from_secs(30),
        output_schema: None,
    };
    let f = fixture(vec![call("remote", json!({ "request": "hello" })), text("ok")], move |b| {
        b.without_default_members().member(member)
    })
    .await;
    assert_eq!(f.harness.agent().info().members, vec!["remote"]);
    f.send("ask the remote agent").await;
    let spawned = f.records("harness.member.spawned");
    assert_eq!(spawned.len(), 1);
    assert_eq!(spawned[0].ext["kind"], "a2a");
    assert_eq!(spawned[0].ext["member"], "remote");
    assert_eq!(f.records("harness.member.finished").len(), 1);
}

/// A `TeamSpec` whose coordinator is a harness compiles its delegate edges: the harness
/// reports that its root consumes runtime toolsets (spec 3.5 §7.19).
#[tokio::test]
async fn a_team_spec_led_by_a_harness_compiles_its_delegate_edges() {
    let dir = tempfile::tempdir().unwrap();
    let harness = |name: &str| -> Arc<dyn Agent> {
        let workspace = dir.path().join(name);
        std::fs::create_dir_all(&workspace).unwrap();
        Arc::new(
            adk_harness::HarnessAgent::builder(name)
                .model(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
                .workspace(adk_devtools::Workspace::new(&workspace))
                .project_instructions(no_user_file())
                .without_default_members()
                .build()
                .unwrap(),
        )
    };
    let (lead, member) = (harness("lead"), harness("member"));
    assert!(lead.capabilities().runtime_tools, "{:?}", lead.capabilities());
    let spec = adk_agent::TeamSpec {
        name: "office".into(),
        description: "the back office".into(),
        coordinator: "lead".into(),
        members: vec![TeamMemberSpec::new("lead"), TeamMemberSpec::new("member")],
        relationships: vec![TeamRelationship::new("lead", "member", RelationshipKind::Delegate)],
        policy: TeamPolicy::default(),
    };
    let team = spec.compile([lead, member]).expect("the delegate edge compiles");
    assert_eq!(team.name(), "office");
}
