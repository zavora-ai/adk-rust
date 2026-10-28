//! Suggested agents (spec 3.3 §5.7, S-I11, S-I12, decisions 20, 23): a suggestion waits in
//! the inbox, only a person's approval of the same definition adds it, within its
//! suggester's limits then; a rejection frees its id; duplicates merge; unattended work
//! suggests at high risk; "Change it first" replaces the suggestion.

mod agents_support;

use std::collections::HashMap;

use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, AgentPrincipal, ApprovalAction, ApprovalDecision, ApprovalRisk, ApprovalState,
    NewRun, RunState,
};
use adk_kernel::agents::{
    AGENT_CREATE_TOOL_NAME, AgentAction, AgentCeiling, AgentDefinition, AgentOrigin, AgentOutcome,
    AgentProposer, AgentRecordState, AgentUses, ShellAccess, WebAccess,
};
use adk_kernel::{PartitionMode, ProcessKind, ProcessSpec, QueueClass, Wake, WakeSet};
use adk_session::{CreateRequest, GetRequest, SessionService};
use agents_support::{APP, PERSON, World, apply, eventually, in_state, local};
use serde_json::json;

const SESSION: &str = "s-1";

fn proposer() -> AgentProposer {
    AgentProposer::new(AgentPrincipal::derived(APP, APP).id, PERSON, SESSION)
}

fn finance() -> AgentDefinition {
    AgentDefinition::new("Finance", "default")
        .with_description("Reads invoices and drafts payment reminders")
        .with_mode(PartitionMode::Ask)
        .with_uses(AgentUses::default().with_web(WebAccess::Any))
}

fn named(name: &str) -> AgentDefinition {
    let mut definition = finance();
    definition.display_name = name.to_string();
    definition
}

fn code(error: adk_kernel::KernelError) -> &'static str {
    AdkError::from(error).code
}

/// Approves request `id` as `by`, a member of the operators.
async fn approve(world: &World, id: &str, by: SubjectRef) -> ApprovalState {
    let request = world.approvals.get(id).await.unwrap().unwrap();
    let decision = ApprovalDecision::approve(by).with_groups(["operator"]);
    world.approvals.decide(id, request.version, decision).await.unwrap().state
}

/// A suggestion adds nothing until a person approves it: no target, no roster row, one
/// inbox request with Approve and Reject and no expiry. An approval by anyone but a person
/// is ignored; a person's adds the agent, hosted on the next tick.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_suggestion_waits_until_a_person_approves_it() {
    let world = World::new();
    let kernel = world.kernel("k1");
    kernel.start().await.unwrap();

    let proposal = kernel.agents().propose(finance(), &proposer()).await.unwrap();
    assert!(!proposal.merged);
    assert_eq!(proposal.agent.state, AgentRecordState::Proposed);
    assert_eq!(proposal.risk, ApprovalRisk::Medium);
    assert_eq!(proposal.request_id, adk_kernel::agents::suggestion_request_id(&proposal.agent));
    assert!(proposal.request_id.starts_with("agent-finance-"));
    let request = world.approvals.get(&proposal.request_id).await.unwrap().unwrap();
    assert_eq!(request.tool_name, AGENT_CREATE_TOOL_NAME);
    assert_eq!(request.summary, "Add an agent called Finance");
    assert_eq!(request.allowed_actions, [ApprovalAction::Approve, ApprovalAction::Reject]);
    assert_eq!(request.expires_at, None);
    assert_eq!(request.requester, Some(SubjectRef::agent(&proposer().agent)));
    let refused = kernel
        .processes()
        .spawn(
            NewRun::new(QueueClass::INTERACTIVE, APP, PERSON, "finance", json!("hi")),
            ProcessSpec::new(ProcessKind::Interactive),
        )
        .await
        .unwrap_err();
    assert!(AdkError::from(refused).message.contains("was not added"));
    assert_eq!(world.actions_about("finance"), [adk_kernel::audit::AGENT_PROPOSED]);

    // An agent in the operators' group approves: ignored, the suggestion stays.
    let other = SubjectRef::agent(&AgentPrincipal::derived(APP, "travel").id);
    assert_eq!(approve(&world, &proposal.request_id, other).await, ApprovalState::Approved);
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    assert_eq!(
        world.store.get("finance").await.unwrap().unwrap().state,
        AgentRecordState::Proposed
    );
    let ignored = world.agent_records().into_iter().last().unwrap();
    assert_eq!(ignored.action, adk_kernel::audit::AGENT_REFUSED);

    // A person approves another suggestion: added on the next tick, then hosted.
    let travel = kernel.agents().propose(named("Travel"), &proposer()).await.unwrap();
    assert_eq!(
        approve(&world, &travel.request_id, SubjectRef::human(PERSON)).await,
        ApprovalState::Approved
    );
    let added = eventually("Travel to be added", || async {
        world
            .store
            .get("travel")
            .await
            .ok()
            .flatten()
            .filter(|r| r.state == AgentRecordState::Active)
    })
    .await;
    assert_eq!(added.approved_by, Some(SubjectRef::human(PERSON)));
    assert!(matches!(added.origin, AgentOrigin::Suggested { unattended: false, .. }));
    let created = world
        .agent_records()
        .into_iter()
        .find(|r| r.action == adk_kernel::audit::AGENT_CREATED && r.ext["id"] == "travel");
    assert_eq!(created.unwrap().ext["approvalId"], json!(travel.request_id));
    let process = kernel
        .processes()
        .spawn(
            NewRun::new(QueueClass::INTERACTIVE, APP, PERSON, "travel", json!("hi"))
                .with_session_id("t-1"),
            ProcessSpec::new(ProcessKind::Interactive),
        )
        .await
        .unwrap();
    in_state(&world.runs, &process.meta.pid, RunState::Completed).await;
    kernel.stop().await.unwrap();
}

/// A rejection deletes the suggestion, and its id, never used, can be given again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rejection_deletes_the_suggestion_and_frees_its_id() {
    let world = World::new();
    let kernel = world.kernel("k1");
    kernel.start().await.unwrap();
    let proposal = kernel.agents().propose(finance(), &proposer()).await.unwrap();
    let request = world.approvals.get(&proposal.request_id).await.unwrap().unwrap();
    let decision = ApprovalDecision::reject(SubjectRef::human(PERSON), "not now");
    world.approvals.decide(&proposal.request_id, request.version, decision).await.unwrap();
    eventually("the suggestion to be deleted", || async {
        world.store.get("finance").await.ok().filter(Option::is_none)
    })
    .await;
    let again = kernel.agents().propose(finance(), &proposer()).await.unwrap();
    assert_eq!(again.agent.id, "finance");
    // Wave 3 findings 5 and 7: suggested again, it has a decision of its own, which the old
    // refusal never answers.
    assert!(!again.merged);
    assert_ne!(again.request_id, proposal.request_id);
    let waiting = world.approvals.get(&again.request_id).await.unwrap().unwrap();
    assert_eq!(waiting.state, ApprovalState::Pending);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        world.store.get("finance").await.unwrap().unwrap().state,
        AgentRecordState::Proposed,
        "the earlier refusal does not end the new suggestion"
    );
    assert_eq!(
        approve(&world, &again.request_id, SubjectRef::human(PERSON)).await,
        ApprovalState::Approved
    );
    eventually("Finance to be added", || async {
        world
            .store
            .get("finance")
            .await
            .ok()
            .flatten()
            .filter(|record| record.state == AgentRecordState::Active)
    })
    .await;
    kernel.stop().await.unwrap();
}

/// Wave 3 finding 6: a suggestion whose decision cannot be written is taken back, so it
/// never holds its name or a slot; suggested again, it waits with a decision.
#[tokio::test]
async fn a_suggestion_whose_decision_cannot_be_written_is_taken_back() {
    let inbox = agents_support::FlakyInbox::new();
    let world = World { approvals: inbox.clone(), ..World::new() };
    let kernel = world.kernel("k1");
    inbox.fail_next_submit.store(true, std::sync::atomic::Ordering::SeqCst);
    let error = kernel.agents().propose(finance(), &proposer()).await.unwrap_err();
    assert!(error.to_string().contains("database is locked"), "{error}");
    assert!(world.store.get("finance").await.unwrap().is_none(), "nothing holds its name");
    assert_eq!(world.actions_about("finance"), [adk_kernel::audit::AGENT_PROPOSED]);
    let again = kernel.agents().propose(finance(), &proposer()).await.unwrap();
    assert!(!again.merged);
    let request = world.approvals.get(&again.request_id).await.unwrap().unwrap();
    assert_eq!(request.state, ApprovalState::Pending);
}

/// Wave 3 finding 21: the suggester's own limits in the turn it suggests in — such as the
/// Suggest mode a run still holds after the person approved its plan — tighten the host's
/// ceiling.
#[tokio::test]
async fn the_suggesters_own_limits_in_its_turn_tighten_the_ceiling() {
    let world = World::new();
    let kernel = world.kernel("k1");
    let held = proposer().with_ceiling(AgentCeiling::new(
        PartitionMode::Plan,
        AgentUses::default().with_web(WebAccess::Any),
    ));
    let error = kernel.agents().propose(finance(), &held).await.unwrap_err();
    assert_eq!(code(error), "kernel.agent.refused");
    let suggests = finance().with_mode(PartitionMode::Plan);
    assert!(kernel.agents().propose(suggests, &held).await.is_ok());
}

/// Wave 3 finding 35: an answer given through a surface adds the agent as that surface, so
/// `kernel.agent.created` names where the person approved it.
#[tokio::test]
async fn an_answer_through_a_surface_adds_the_agent_as_that_surface() {
    let world = World::new();
    let kernel = world.kernel("k1");
    let proposal = kernel.agents().propose(finance(), &proposer()).await.unwrap();
    approve(&world, &proposal.request_id, SubjectRef::human(PERSON)).await;
    let phone = adk_kernel::agents::AgentCaller::new(SubjectRef::human(PERSON), "phone", "touch")
        .with_device("phone-1")
        .with_presence(adk_kernel::agents::Presence::Confirmed);
    kernel.agents().answered(&proposal.request_id, &phone).await;
    let added = world.store.get("finance").await.unwrap().unwrap();
    assert_eq!(added.state, AgentRecordState::Active);
    assert_eq!(added.changed_on.as_deref(), Some("phone"));
    let created = world
        .agent_records()
        .into_iter()
        .find(|record| record.action == adk_kernel::audit::AGENT_CREATED)
        .unwrap();
    assert_eq!(
        (&created.ext["surface"], &created.ext["device"], &created.ext["presence"]),
        (&json!("phone"), &json!("phone-1"), &json!("confirmed"))
    );
}

/// A suggestion over its suggester's limits is refused when made (S-I12), and one whose
/// suggester lost a limit since is refused when approved, with a notice in the suggesting
/// thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_ceiling_is_checked_when_made_and_again_when_approved() {
    let world = World::new();
    world
        .sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: PERSON.into(),
            session_id: Some(SESSION.into()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let kernel = world.kernel("k1");
    kernel.start().await.unwrap();
    for over in [
        finance().with_uses(AgentUses::default().with_shell(ShellAccess::Host)),
        finance().with_mode(PartitionMode::Auto),
        finance().with_uses(AgentUses::default().with_pack("payments")),
    ] {
        let error = kernel.agents().propose(over, &proposer()).await.unwrap_err();
        assert_eq!(code(error), "kernel.agent.refused");
    }
    assert!(world.store.list().await.unwrap().is_empty());

    let proposal = kernel.agents().propose(finance(), &proposer()).await.unwrap();
    *world.factory.ceiling.lock().unwrap() =
        Some(AgentCeiling::new(PartitionMode::Ask, AgentUses::default()));
    approve(&world, &proposal.request_id, SubjectRef::human(PERSON)).await;
    let refused = eventually("the suggestion to end refused", || async {
        world
            .store
            .get("finance")
            .await
            .ok()
            .flatten()
            .filter(|r| r.state == AgentRecordState::Refused)
    })
    .await;
    assert_eq!(refused.version, 2);
    // The notice follows the refused record, so it can land a moment after it.
    let notice = eventually("the notice in the suggesting thread", || async {
        let session = world
            .sessions
            .get(GetRequest {
                app_name: APP.into(),
                user_id: PERSON.into(),
                session_id: SESSION.into(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap();
        session
            .events()
            .all()
            .into_iter()
            .filter_map(|event| event.llm_response.content.clone())
            .filter_map(|content| {
                content.parts.first().and_then(|part| part.text().map(str::to_string))
            })
            .next_back()
            .filter(|text| text.contains("wasn't added"))
    })
    .await;
    assert!(
        notice.contains(
            "Finance wasn't added: the agent that suggested it can no longer give it any site"
        ),
        "{notice}"
    );
    kernel.stop().await.unwrap();
}

/// The same suggestion merges into the one waiting, and a fourth open suggestion from one
/// agent is refused.
#[tokio::test]
async fn duplicates_merge_and_a_fourth_open_suggestion_is_refused() {
    let world = World::new();
    let kernel = world.kernel("k1");
    let first = kernel.agents().propose(finance(), &proposer()).await.unwrap();
    let again = kernel.agents().propose(finance(), &proposer()).await.unwrap();
    assert!(again.merged);
    assert_eq!((again.agent.id, again.request_id), (first.agent.id, first.request_id));
    kernel.agents().propose(named("Travel"), &proposer()).await.unwrap();
    kernel.agents().propose(named("Garden"), &proposer()).await.unwrap();
    let error = kernel.agents().propose(named("Music"), &proposer()).await.unwrap_err();
    assert_eq!(code(error), "kernel.agent.refused");
    assert_eq!(world.store.list().await.unwrap().len(), 3);
}

/// A suggestion from work the person did not start at a screen — a routine, or a turn a
/// chat message started — is high risk (decision 20); one from an interactive turn keeps the
/// definition's risk.
#[tokio::test]
async fn unattended_and_chat_started_suggestions_are_high_risk() {
    let world = World::new();
    let kernel = world.kernel("k1");
    let spawn = |kind: ProcessKind, input: serde_json::Value, wake: Option<WakeSet>| {
        let kernel = kernel.clone();
        async move {
            let mut spec = ProcessSpec::new(kind);
            if let Some(wake) = wake {
                spec = spec.with_wake(wake);
            }
            let run = NewRun::new(QueueClass::INTERACTIVE, APP, PERSON, APP, input)
                .with_session_id(format!("s-{}", uuid::Uuid::new_v4().simple()));
            kernel.processes().spawn(run, spec).await.unwrap().meta.pid
        }
    };
    let routine =
        spawn(ProcessKind::Scheduled, json!("tidy"), Some(WakeSet::any_of([Wake::Manual]))).await;
    let chat = spawn(
        ProcessKind::Interactive,
        json!({ "text": "hi", "channel": { "kind": "chat" } }),
        None,
    )
    .await;
    let screen = spawn(ProcessKind::Interactive, json!("hi"), None).await;
    for (name, pid, risk) in [
        ("Routine", routine, ApprovalRisk::High),
        ("Chat", chat, ApprovalRisk::High),
        ("Screen", screen, ApprovalRisk::Medium),
    ] {
        let proposal =
            kernel.agents().propose(named(name), &proposer().with_run(pid)).await.unwrap();
        assert_eq!(proposal.risk, risk, "{name}");
        let request = world.approvals.get(&proposal.request_id).await.unwrap().unwrap();
        assert_eq!(request.risk, risk, "{name}");
    }
}

/// "Change it first": the person adds the suggestion by hand with their changes; the
/// suggestion ends as "You changed it and added it" and the agent keeps its id.
#[tokio::test]
async fn change_it_first_replaces_the_suggestion() {
    let world = World::new();
    let kernel = world.kernel("k1");
    let proposal = kernel.agents().propose(finance(), &proposer()).await.unwrap();
    let by_hand = AgentAction::Create {
        definition: finance().with_description("Only reads invoices"),
        replaces: Some("finance".into()),
    };
    let Ok(AgentOutcome::Created { agent }) = apply(&kernel, &local(), by_hand).await else {
        panic!("not added");
    };
    assert_eq!((agent.id.as_str(), agent.origin.clone()), ("finance", AgentOrigin::ByHand));
    let request = world.approvals.get(&proposal.request_id).await.unwrap().unwrap();
    assert_eq!(request.state, ApprovalState::Cancelled);
    assert_eq!(request.note.as_deref(), Some("You changed it and added it"));
    // Without replacing it, the same name is refused while the suggestion waits.
    let waiting = kernel.agents().propose(named("Travel"), &proposer()).await.unwrap();
    let clash = AgentAction::Create { definition: named("Travel"), replaces: None };
    let error = kernel.agents().preview(&local(), &clash).await.unwrap_err();
    assert!(AdkError::from(error).message.contains("is waiting for your answer"));
    assert_eq!(waiting.agent.state, AgentRecordState::Proposed);
}
