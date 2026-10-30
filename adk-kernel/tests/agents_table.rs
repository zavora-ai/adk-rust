//! The kernel's agent table (spec 3.3 §5.7, §10 K7, S-I11, S-I14, S-I16, S-I18): adding by
//! hand through a preview and its digest, who may change agents, names and ids, versions,
//! the record before the act, pausing, removing, and the `target` process filter.

mod agents_support;

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditFailureMode, AuditRecordDraft, AuditRecorder, AuditTrail};
use adk_core::principal::SubjectRef;
use adk_core::{
    ActingContext, AdkError, AgentPrincipal, ApprovalPolicy, ApprovalRequest, ApprovalRisk,
    ApprovalScope, ApprovalState, ApproverSelector, NewRun, RunState, ToolConfirmationRequest,
};
use adk_kernel::agents::{
    AgentAction, AgentCaller, AgentChange, AgentDefinition, AgentOutcome, AgentRecordState,
    AgentUses, FileAccess, Presence,
};
use adk_kernel::{
    AgendaItem, AgendaOrigin, AgendaState, KernelEvent, PartitionMode, ProcessFilter, ProcessKind,
    ProcessSpec, QueueClass, Suspension, Wake, WakeSet,
};
use agents_support::{APP, PERSON, World, add, apply, eventually, in_state, local, web};
use serde_json::json;

fn code(error: adk_kernel::KernelError) -> &'static str {
    AdkError::from(error).code
}

fn finance() -> AgentDefinition {
    AgentDefinition::new("Finance", "default")
        .with_description("Reads invoices and drafts payment reminders")
        .with_instructions("Draft reminders; never send them.")
        .with_mode(PartitionMode::Ask)
        .with_uses(AgentUses::default().with_files(FileAccess::Read).with_folder("Invoices"))
}

/// A run of `target` in its own session.
fn run(target: &str, session: &str) -> NewRun {
    NewRun::new(QueueClass::INTERACTIVE, APP, PERSON, target, json!("hello"))
        .with_session_id(session)
}

/// A process of `target` asleep until woken by hand.
async fn sleeper(kernel: &adk_kernel::Kernel, target: &str, kind: ProcessKind) -> String {
    let spec = ProcessSpec::new(kind).with_wake(WakeSet::any_of([Wake::Manual]));
    let session = format!("s-{}", uuid::Uuid::new_v4().simple());
    let mut new_run = run(target, &session);
    new_run.queue = QueueClass::BACKGROUND.to_string();
    kernel.processes().spawn(new_run, spec).await.unwrap().meta.pid
}

/// A decision `target`'s process `pid` waits on, in its session.
async fn decision(world: &World, kernel: &adk_kernel::Kernel, target: &str, pid: &str) -> String {
    let process = kernel.processes().get(pid).await.unwrap().unwrap();
    let session = process.run.session_id.clone().unwrap();
    let call = ToolConfirmationRequest::new("send_email", json!({ "to": "client@example.com" }));
    let request = ApprovalRequest::new(
        format!("held-{}", uuid::Uuid::new_v4().simple()),
        ApprovalScope::new(APP, PERSON, session),
        &call,
        ApprovalPolicy::new([ApproverSelector::human(PERSON), ApproverSelector::group("operator")]),
    )
    .with_risk(ApprovalRisk::Medium)
    .with_run_id(pid)
    .with_acting(ActingContext::new(AgentPrincipal::derived(APP, target)));
    world.approvals.submit(request).await.unwrap().id
}

/// Adding by hand previews first, applies only with the preview's digest, writes
/// `kernel.agent.created` with the caller first, and hosts the agent at once: its run
/// completes on its own runner.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adding_by_hand_previews_then_applies_and_hosts_at_once() {
    let world = World::new();
    let kernel = world.kernel("k1");
    kernel.start().await.unwrap();
    let mut events = kernel.events();
    let action = AgentAction::Create { definition: finance(), replaces: None };

    let preview = kernel.agents().preview(&local(), &action).await.unwrap();
    assert_eq!(preview.id, "finance");
    assert_eq!((preview.risk, preview.money, preview.widens), (ApprovalRisk::Medium, false, true));
    assert_eq!(preview.code().len(), 6);
    assert!(preview.abilities.contains(&"Finance answers in its own threads.".to_string()));
    assert!(kernel.agents().get("finance").await.unwrap().is_none(), "a preview keeps nothing");

    let wrong = kernel.agents().apply(&local(), action.clone(), "0000").await.unwrap_err();
    assert_eq!(code(wrong), "kernel.agent.unconfirmed");
    let Ok(AgentOutcome::Created { agent }) =
        kernel.agents().apply(&local(), action, &preview.digest).await
    else {
        panic!("not created");
    };
    assert_eq!(
        (agent.id.as_str(), agent.state, agent.version),
        ("finance", AgentRecordState::Active, 1)
    );
    assert_eq!(agent.created_by, SubjectRef::human(PERSON));

    // The apply with another digest is refused on record (spec 3.3 §11), then the agent added.
    let records = world.agent_records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].action, adk_kernel::audit::AGENT_REFUSED);
    let created = &records[1];
    assert_eq!(created.action, adk_kernel::audit::AGENT_CREATED);
    assert_eq!(created.ext["surface"], "terminal");
    assert_eq!(created.ext["transport"], "socket");
    assert_eq!(created.ext["presence"], "pairedLocal");
    assert_eq!(created.ext["peerPid"], 4242);
    assert_eq!(created.ext["digest"], json!(agent.digest));

    let change = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let record = events.recv().await.expect("the bus stays open");
            if let KernelEvent::Agent { id, change, .. } = record.event
                && id == "finance"
            {
                return change;
            }
        }
    })
    .await
    .expect("the created event");
    assert_eq!(change, AgentChange::Created);

    let process = kernel
        .processes()
        .spawn(run("finance", "finance-1"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap();
    let done = in_state(&world.runs, &process.meta.pid, RunState::Completed).await;
    assert_eq!(done.target, "finance");
    let listed = kernel.agents().list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].cant_start, None);
    assert_eq!(kernel.agents().choices().await.unwrap().models.len(), 1);
    kernel.stop().await.unwrap();
}

/// Only a person changes agents (S-I11), never through a device, and high risk or money
/// needs presence.
#[tokio::test]
async fn the_table_refuses_who_is_not_a_person_and_high_risk_without_presence() {
    let world = World::new();
    let kernel = world.kernel("k1");
    let action = AgentAction::Create { definition: finance(), replaces: None };
    let agent = AgentCaller::new(
        SubjectRef::agent(&AgentPrincipal::derived(APP, APP).id),
        "terminal",
        "keyboard",
    );
    let service = AgentCaller::new(SubjectRef::service("scheduler"), "web", "pointer");
    let device = web().with_device("phone-1");
    for caller in [&agent, &service, &device] {
        let error = kernel.agents().preview(caller, &action).await.unwrap_err();
        assert_eq!(code(error), "kernel.agent.refused");
        let error =
            kernel.agents().apply(caller, action.clone(), &action.digest()).await.unwrap_err();
        assert_eq!(code(error), "kernel.agent.refused");
    }

    let named = |name: &str, mut definition: AgentDefinition| {
        definition.display_name = name.to_string();
        definition
    };
    for risky in [
        named("Ops", finance().with_mode(PartitionMode::Auto)),
        named("Shopper", finance().with_uses(AgentUses::default().with_pack("payments"))),
    ] {
        let action = AgentAction::Create { definition: risky, replaces: None };
        let preview = kernel.agents().preview(&web(), &action).await.unwrap();
        assert!(preview.needs_presence(), "{preview:?}");
        let error =
            kernel.agents().apply(&web(), action.clone(), &preview.digest).await.unwrap_err();
        assert_eq!(code(error), "kernel.agent.refused");
        let confirmed = web().with_presence(Presence::Confirmed);
        assert!(kernel.agents().apply(&confirmed, action, &preview.digest).await.is_ok());
    }
    assert!(world.agent_records().iter().any(|r| r.action == adk_kernel::audit::AGENT_REFUSED));
    assert_eq!(kernel.agents().list().await.unwrap().len(), 2);
}

/// Names are unique, never "You" or the primary agent's; ids are given once, so a new
/// Finance after a removal gets `finance-2` (S-I14).
#[tokio::test]
async fn names_are_unique_and_a_removed_id_is_never_given_again() {
    let world = World::new();
    let kernel = world.kernel("k1");
    let first = add(&kernel, finance()).await;
    for name in ["FINANCE", "You", "ADK-Rust", "adk-rust"] {
        let mut taken = finance();
        taken.display_name = name.into();
        let action = AgentAction::Create { definition: taken, replaces: None };
        let error = kernel.agents().preview(&local(), &action).await.unwrap_err();
        assert_eq!(code(error), "kernel.agent.invalid", "{name}");
    }
    let remove = AgentAction::Remove { id: first.id.clone(), expected_version: first.version };
    assert!(matches!(
        apply(&kernel, &local(), remove).await.unwrap(),
        AgentOutcome::Removed { .. }
    ));
    let second = add(&kernel, finance()).await;
    assert_eq!(second.id, "finance-2");
    assert_eq!(
        kernel.agents().get("finance").await.unwrap().unwrap().agent.state,
        AgentRecordState::Removed
    );
}

/// A change names what it touches, applies only against the version it was made on, keeps
/// the area, and when it lets the agent do more ends its waiting decisions first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changes_compare_versions_and_widening_ends_waiting_decisions() {
    let world = World::new();
    let kernel = world.kernel("k1");
    let finance_v1 = add(&kernel, finance().with_mode(PartitionMode::Plan)).await;
    let pid = sleeper(&kernel, "finance", ProcessKind::Interactive).await;
    let waiting = decision(&world, &kernel, "finance", &pid).await;

    let mut bills = finance_v1.definition().with_mode(PartitionMode::ReadOnly);
    bills.display_name = "Bills".into();
    let rename = AgentAction::Change {
        id: "finance".into(),
        expected_version: 1,
        definition: bills.clone(),
    };
    let preview = kernel.agents().preview(&web(), &rename).await.unwrap();
    assert_eq!(preview.fields, ["displayName", "mode"]);
    assert!(!preview.widens);
    assert_eq!(preview.risk, ApprovalRisk::Low);
    let Ok(AgentOutcome::Changed { agent }) =
        kernel.agents().apply(&web(), rename.clone(), &preview.digest).await
    else {
        panic!("not changed");
    };
    assert_eq!((agent.display_name.as_str(), agent.version), ("Bills", 2));
    assert_eq!(
        world.approvals.get(&waiting).await.unwrap().unwrap().state,
        ApprovalState::Pending,
        "doing less ends nothing"
    );

    let stale = kernel.agents().preview(&web(), &rename).await.unwrap_err();
    let message = AdkError::from(stale).message;
    assert!(message.contains("Bills was changed at") && message.contains("version 2"), "{message}");

    let moved = AgentAction::Change {
        id: "finance".into(),
        expected_version: 2,
        definition: {
            let mut moved = bills.clone();
            moved.area = "work".into();
            moved
        },
    };
    assert_eq!(
        code(kernel.agents().preview(&web(), &moved).await.unwrap_err()),
        "kernel.agent.invalid"
    );

    let wider = AgentAction::Change {
        id: "finance".into(),
        expected_version: 2,
        definition: bills.with_mode(PartitionMode::Ask),
    };
    let preview = kernel.agents().preview(&web(), &wider).await.unwrap();
    assert!(preview.widens);
    assert_eq!(preview.risk, ApprovalRisk::Medium);
    kernel.agents().apply(&web(), wider, &preview.digest).await.unwrap();
    let ended = world.approvals.get(&waiting).await.unwrap().unwrap();
    assert_eq!(
        ended.state,
        ApprovalState::Cancelled,
        "doing more ends its waiting decisions first"
    );
    assert_eq!(
        world.actions_about("finance"),
        [
            adk_kernel::audit::AGENT_CREATED,
            adk_kernel::audit::AGENT_CHANGED,
            adk_kernel::audit::AGENT_CHANGED
        ]
    );
    let changed = &world.agent_records()[1];
    assert_eq!(changed.ext["fields"], json!(["displayName", "mode"]));
    assert_eq!(changed.ext["from"]["mode"], "plan");
    assert_eq!(changed.ext["to"]["mode"], "read_only");
}

/// A recorder that fails every write.
struct Broken;

#[async_trait::async_trait]
impl AuditRecorder for Broken {
    fn digest(&self, _value: &serde_json::Value) -> String {
        String::new()
    }

    async fn record(&self, _draft: AuditRecordDraft) -> adk_core::Result<()> {
        Err(AdkError::new(
            adk_core::ErrorComponent::Auth,
            adk_core::ErrorCategory::Unavailable,
            "audit.down",
            "the audit log is down",
        ))
    }
}

/// The record is written before the change, and under `AuditFailureMode::Block` a change
/// whose record cannot be written does not happen.
#[tokio::test]
async fn a_change_whose_record_cannot_be_written_does_not_happen() {
    let world = World::new();
    let kernel = world.kernel_with("k1", |builder| {
        builder.audit(AuditTrail::new(Arc::new(Broken)).with_failure_mode(AuditFailureMode::Block))
    });
    let action = AgentAction::Create { definition: finance(), replaces: None };
    let preview = kernel.agents().preview(&local(), &action).await.unwrap();
    let error = kernel.agents().apply(&local(), action, &preview.digest).await.unwrap_err();
    assert_eq!(code(error), "audit.record_failed");
    assert!(world.store.list().await.unwrap().is_empty(), "nothing was stored");
    assert_eq!(
        world.factory.builds.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "nothing was built"
    );
}

/// Pausing marks its work paused and refuses new work; a freeze and its lifting leave it
/// paused (K-I21); resuming wakes only what the pause stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_holds_its_work_through_an_unfreeze_and_resume_wakes_only_that() {
    let world = World::new();
    let kernel = world.kernel("k1");
    kernel.start().await.unwrap();
    add(&kernel, finance()).await;
    let held = sleeper(&kernel, "finance", ProcessKind::Scheduled).await;
    let manual = sleeper(&kernel, "finance", ProcessKind::Scheduled).await;
    in_state(&world.runs, &held, RunState::Waiting).await;
    in_state(&world.runs, &manual, RunState::Waiting).await;
    kernel.processes().suspend(&manual).await.unwrap();

    let paused = kernel.agents().pause(&web(), "finance").await.unwrap();
    assert_eq!(paused.state, AgentRecordState::Paused);
    let meta = kernel.processes().get(&held).await.unwrap().unwrap().meta;
    assert_eq!(meta.suspended_by, Some(Suspension::Paused { agent: "finance".into() }));
    let kept = kernel.processes().get(&manual).await.unwrap().unwrap().meta;
    assert_eq!(kept.suspended_by, Some(Suspension::Manual), "a person's own suspension stays");
    let refused = kernel
        .processes()
        .spawn(run("finance", "f-2"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap_err();
    let message = AdkError::from(refused).message;
    assert!(message.starts_with("Finance is paused"), "{message}");

    kernel.freeze("incident", adk_core::audit_log::AuditActor::default()).await.unwrap();
    kernel.unfreeze(adk_core::audit_log::AuditActor::default()).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let still = kernel.processes().get(&held).await.unwrap().unwrap();
    assert_eq!(still.run.state, RunState::Waiting);
    assert_eq!(still.meta.suspended_by, Some(Suspension::Paused { agent: "finance".into() }));

    kernel.agents().resume(&local(), "finance").await.unwrap();
    let woken = eventually("the paused process to be woken", || async {
        kernel.processes().get(&held).await.ok().flatten().filter(|p| p.meta.suspended_by.is_none())
    })
    .await;
    assert_eq!(woken.meta.suspended_by, None);
    let kept = kernel.processes().get(&manual).await.unwrap().unwrap().meta;
    assert_eq!(
        kept.suspended_by,
        Some(Suspension::Manual),
        "resume wakes only what the pause stopped"
    );
    assert_eq!(
        world.actions_about("finance"),
        [
            adk_kernel::audit::AGENT_CREATED,
            adk_kernel::audit::AGENT_PAUSED,
            adk_kernel::audit::AGENT_RESUMED
        ]
    );
    kernel.stop().await.unwrap();
}

/// Removing names what ends before it acts, then ends its work, routines, waiting decisions
/// and follow-ups, revokes its rules, retires its target and keeps a tombstone (S-I16).
#[tokio::test]
async fn removing_ends_work_routines_decisions_and_follow_ups() {
    let world = World::new();
    let kernel = world.kernel("k1");
    let finance = add(&kernel, finance()).await;
    let work = kernel
        .processes()
        .spawn(run("finance", "f-work"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap()
        .meta
        .pid;
    let routine = sleeper(&kernel, "finance", ProcessKind::Scheduled).await;
    let other = kernel
        .processes()
        .spawn(run(APP, "primary"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap()
        .meta
        .pid;
    let waiting = decision(&world, &kernel, "finance", &work).await;
    let item = AgendaItem::new(
        "follow-up-1",
        SubjectRef::human(PERSON),
        "Chase the invoice",
        "It is overdue",
        AgendaOrigin::Agent { pid: work.clone(), confidence: 0.9 },
        chrono::Utc::now(),
    );
    kernel.agenda().propose(item, PartitionMode::Ask).await.unwrap();

    let remove = AgentAction::Remove { id: "finance".into(), expected_version: finance.version };
    let preview = kernel.agents().preview(&local(), &remove).await.unwrap();
    let named = preview.ended.clone().unwrap();
    assert_eq!(named.work, std::slice::from_ref(&work));
    assert_eq!(named.routines, std::slice::from_ref(&routine));
    assert_eq!(named.decisions, std::slice::from_ref(&waiting));
    assert_eq!(named.follow_ups, ["follow-up-1"]);

    let Ok(AgentOutcome::Removed { agent, ended }) =
        kernel.agents().apply(&local(), remove, &preview.digest).await
    else {
        panic!("not removed");
    };
    assert_eq!((agent.state, agent.version), (AgentRecordState::Removed, 3));
    assert_eq!(
        (ended.work.clone(), ended.routines.clone(), ended.rules),
        (named.work, named.routines, 2)
    );
    for pid in [&work, &routine] {
        assert_eq!(world.runs.get(pid).await.unwrap().unwrap().state, RunState::Cancelled);
    }
    assert_ne!(world.runs.get(&other).await.unwrap().unwrap().state, RunState::Cancelled);
    let request = world.approvals.get(&waiting).await.unwrap().unwrap();
    assert_eq!(request.state, ApprovalState::Cancelled, "ended without an answer");
    assert!(
        request.note.as_deref().is_some_and(|note| note.starts_with("You removed Finance at ")),
        "{:?}",
        request.note
    );
    let follow_up = kernel.agenda().get("follow-up-1").await.unwrap().unwrap();
    assert_eq!(follow_up.state, AgendaState::Rejected);
    assert_eq!(*world.factory.retired.lock().unwrap(), ["finance"]);
    let refused = kernel
        .processes()
        .spawn(run("finance", "f-3"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap_err();
    assert!(AdkError::from(refused).message.contains("was removed"));
    assert_eq!(
        world.actions_about("finance"),
        [
            adk_kernel::audit::AGENT_CREATED,
            adk_kernel::audit::AGENT_REMOVING,
            adk_kernel::audit::AGENT_REMOVED
        ]
    );
    let removed = world.agent_records().into_iter().last().unwrap();
    assert_eq!(removed.ext["ended"]["decisions"], json!([waiting]));
}

/// A removal interrupted after its record is finished by the next tick on any replica.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_interrupted_removal_is_finished_by_the_next_tick() {
    let world = World::new();
    let kernel = world.kernel("k1");
    let finance = add(&kernel, finance()).await;
    let mut removing = finance.clone().with_state(AgentRecordState::Removing);
    removing.version = 2;
    assert!(world.store.put_if_version(removing, 1).await.unwrap());

    let replica = world.kernel("k2");
    replica.start().await.unwrap();
    let tombstone = eventually("the removal to finish", || async {
        world
            .store
            .get("finance")
            .await
            .ok()
            .flatten()
            .filter(|r| r.state == AgentRecordState::Removed)
    })
    .await;
    assert_eq!(tombstone.version, 3);
    assert!(world.factory.retired.lock().unwrap().contains(&"finance".to_string()));
    replica.stop().await.unwrap();
}

/// The tick leaves a removal a person is finishing to them, however long revoking its rules
/// takes: its rules are revoked once, by the removal whose outcome counts them, and it is
/// recorded removed once (S-I16, S-I18).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_tick_leaves_a_removal_in_progress_to_the_person() {
    let world = World::new();
    let kernel = world.kernel("k1");
    kernel.start().await.unwrap();
    let finance = add(&kernel, finance()).await;
    // Ten ticks pass while its rules are revoked.
    *world.factory.retire_delay.lock().unwrap() = Some(Duration::from_millis(500));

    let remove = AgentAction::Remove { id: "finance".into(), expected_version: finance.version };
    let Ok(AgentOutcome::Removed { agent, .. }) = apply(&kernel, &local(), remove).await else {
        panic!("not removed");
    };
    assert_eq!((agent.state, agent.version), (AgentRecordState::Removed, 3));
    assert_eq!(*world.factory.retired.lock().unwrap(), ["finance"]);
    assert_eq!(
        world.actions_about("finance"),
        [
            adk_kernel::audit::AGENT_CREATED,
            adk_kernel::audit::AGENT_REMOVING,
            adk_kernel::audit::AGENT_REMOVED
        ]
    );
    kernel.stop().await.unwrap();
}

/// `ProcessFilter::target` lists one agent's processes.
#[tokio::test]
async fn the_target_filter_lists_one_agents_processes() {
    let world = World::new();
    let kernel = world.kernel("k1");
    add(&kernel, finance()).await;
    let mut travel = finance();
    travel.display_name = "Travel".into();
    add(&kernel, travel).await;
    let mine = kernel
        .processes()
        .spawn(run("finance", "a"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap()
        .meta
        .pid;
    kernel
        .processes()
        .spawn(run("travel", "b"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap();
    kernel
        .processes()
        .spawn(run(APP, "c"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap();
    let listed: Vec<String> = kernel
        .processes()
        .ps(ProcessFilter::default().target("finance"))
        .await
        .unwrap()
        .into_iter()
        .map(|process| process.run.run_id)
        .collect();
    assert_eq!(listed, [mine]);
}

/// Wave 3 findings 17–19: an agent paused by hand is resumed in the web home or on the
/// kernel's own terminal (S-I10), never from a terminal over HTTP or the phone.
#[tokio::test]
async fn a_paused_agent_resumes_only_in_the_home_or_on_the_local_terminal() {
    let world = World::new();
    let kernel = world.kernel("k1");
    add(&kernel, finance()).await;
    kernel.agents().pause(&local(), "finance").await.unwrap();
    let remote = AgentCaller::new(SubjectRef::human(PERSON), "terminal", "keyboard")
        .with_transport(adk_kernel::control::ControlTransport::Http);
    let phone = AgentCaller::new(SubjectRef::human(PERSON), "phone", "touch");
    for caller in [&remote, &phone] {
        let error = kernel.agents().resume(caller, "finance").await.unwrap_err();
        assert_eq!(code(error), "kernel.agent.refused", "{caller:?}");
    }
    kernel.agents().resume(&web(), "finance").await.unwrap();
    kernel.agents().pause(&web(), "finance").await.unwrap();
    kernel.agents().resume(&local(), "finance").await.unwrap();
}

/// Wave 3 finding 26: no agent gets the id `new`, which the web home's New agent screen
/// uses.
#[tokio::test]
async fn an_agent_named_new_never_gets_the_new_agent_screens_id() {
    let world = World::new();
    let kernel = world.kernel("k1");
    let mut definition = finance();
    definition.display_name = "New".into();
    assert_eq!(add(&kernel, definition).await.id, "new-2");
}

/// Wave 3 finding 31: a kernel without areas checks an agent against the mode its primary
/// agent started in, not "Go ahead".
#[tokio::test]
async fn the_default_area_is_checked_at_the_mode_the_kernel_started_in() {
    let world = World::new();
    let control = adk_kernel::control::ControlPlaneConfig::default()
        .without_socket()
        .with_start_mode(adk_kernel::PartitionId::default(), PartitionMode::Plan);
    let kernel = world.kernel_with_control("k1", control, |builder| builder);
    let area = kernel.agents().area(&adk_kernel::PartitionId::default()).await.unwrap();
    assert_eq!(area.default_mode, PartitionMode::Plan);
    let asks = AgentAction::Create { definition: finance(), replaces: None };
    let error = kernel.agents().preview(&local(), &asks).await.unwrap_err();
    assert_eq!(code(error), "kernel.agent.refused");
    let suggests = AgentAction::Create {
        definition: finance().with_mode(PartitionMode::Plan),
        replaces: None,
    };
    assert!(kernel.agents().preview(&local(), &suggests).await.is_ok());
}

/// Wave 3 finding 33: a change that lets an agent do more but loses its write to another
/// change ends none of the agent's waiting decisions.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_widening_change_that_loses_its_write_ends_no_decision() {
    let racing = Arc::new(agents_support::RacingStore {
        inner: Arc::new(adk_kernel::stores::InMemoryAgentStore::new()),
        lose_next_write: std::sync::atomic::AtomicBool::new(false),
    });
    let world = World { store: racing.clone(), ..World::new() };
    let kernel = world.kernel("k1");
    let added = add(&kernel, finance().with_mode(PartitionMode::Plan)).await;
    let pid = sleeper(&kernel, "finance", ProcessKind::Interactive).await;
    let waiting = decision(&world, &kernel, "finance", &pid).await;
    let wider = AgentAction::Change {
        id: "finance".into(),
        expected_version: added.version,
        definition: added.definition().with_mode(PartitionMode::Ask),
    };
    let preview = kernel.agents().preview(&web(), &wider).await.unwrap();
    assert!(preview.widens);
    racing.lose_next_write.store(true, std::sync::atomic::Ordering::SeqCst);
    let error = kernel.agents().apply(&web(), wider, &preview.digest).await.unwrap_err();
    assert_eq!(code(error), "kernel.agent.out_of_date");
    assert_eq!(
        world.approvals.get(&waiting).await.unwrap().unwrap().state,
        ApprovalState::Pending,
        "the change never happened, so its decisions keep waiting"
    );
}

/// Wave 3 findings 8 and 32: instructions set up by hand are kept as the host screens them,
/// and every change notes the surface it was made on, which an out-of-date refusal names
/// (finding 37).
#[tokio::test]
async fn instructions_by_hand_are_screened_and_changes_note_their_surface() {
    let world = World::new();
    let kernel = world.kernel("k1");
    let secret = finance()
        .with_instructions(format!("Use key {} for the bank.", agents_support::TEST_SECRET));
    let added = add(&kernel, secret).await;
    assert_eq!(added.instructions, "Use key [REDACTED] for the bank.");
    assert_eq!(added.changed_on.as_deref(), Some("terminal"));
    let stored = world.store.get("finance").await.unwrap().unwrap();
    assert!(!stored.instructions.contains(agents_support::TEST_SECRET));
    let rename = AgentAction::Change {
        id: "finance".into(),
        expected_version: added.version,
        definition: {
            let mut bills = added.definition();
            bills.display_name = "Bills".into();
            bills
        },
    };
    let Ok(AgentOutcome::Changed { agent }) = apply(&kernel, &web(), rename).await else {
        panic!("not changed");
    };
    assert_eq!(agent.changed_on.as_deref(), Some("web"));
    assert_eq!(world.agent_records()[1].ext["fields"], json!(["displayName"]));
}
