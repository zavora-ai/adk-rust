//! The agent table at spec 3.3's gate (§5.7, §10 K7, §11 S-I12, S-I16, S-I18): a suggestion
//! spends at most its suggester and comes from a known area, the approved digest must be the
//! suggested one, a by-hand change with another digest is refused on record, a removal the
//! tick finishes names the tick, and every `kernel.agent.*` record carries its fields.

use crate::agents_support;

use std::sync::Arc;

use adk_core::audit_log::AuditActor;
use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, AgentPrincipal, ApprovalDecision, ApprovalRisk, ApprovalState, ApprovalStore,
    InMemoryApprovalStore, NewRun, StoreApprovalService,
};
use adk_kernel::agents::{
    AgentAction, AgentCaller, AgentCeiling, AgentDefinition, AgentOrigin, AgentProposer,
    AgentRecordState, AgentUses, FileAccess, Presence, WebAccess,
};
use adk_kernel::control::ControlTransport;
use adk_kernel::{Partition, PartitionMode, ProcessKind, ProcessSpec, QueueClass};
use agents_support::{APP, PERSON, World, add, apply, eventually, local};
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

fn code(error: adk_kernel::KernelError) -> &'static str {
    AdkError::from(error).code
}

/// Approves request `id` as the person, a member of the operators.
async fn approve(world: &World, id: &str) -> ApprovalState {
    let request = world.approvals.get(id).await.unwrap().unwrap();
    let decision = ApprovalDecision::approve(SubjectRef::human(PERSON)).with_groups(["operator"]);
    world.approvals.decide(id, request.version, decision).await.unwrap().state
}

/// An interactive turn of the primary agent, as the kernel runs one a screen started.
async fn turn(kernel: &adk_kernel::Kernel) -> String {
    let run = NewRun::new(QueueClass::INTERACTIVE, APP, PERSON, APP, json!("hi"))
        .with_session_id(format!("s-{}", uuid::Uuid::new_v4().simple()));
    kernel
        .processes()
        .spawn(run, ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap()
        .meta
        .pid
}

/// A suggestion spends at most what its suggester may (S-I12): one over the suggester's
/// daily limit is refused, one naming no limit is kept with the suggester's, and one whose
/// suggester's limit fell since is refused when approved. Fails on `origin/main`, where the
/// ceiling carries no spending and both suggestions are kept as asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_suggestion_spends_at_most_its_suggester() {
    let world = World::new();
    *world.factory.ceiling.lock().unwrap() = Some(
        AgentCeiling::new(PartitionMode::Ask, AgentUses::default().with_web(WebAccess::Any))
            .with_daily_limit(2_000_000),
    );
    let kernel = world.kernel("k1");
    kernel.start().await.unwrap();

    let over = kernel
        .agents()
        .propose(finance().with_daily_limit(5_000_000), &proposer())
        .await
        .unwrap_err();
    let message = AdkError::from(over).message;
    assert!(message.contains("a daily spending limit of $5.00, above its $2.00"), "{message}");

    let proposal = kernel.agents().propose(finance(), &proposer()).await.unwrap();
    assert_eq!(proposal.agent.daily_limit_micro_usd, Some(2_000_000));
    let request = world.approvals.get(&proposal.request_id).await.unwrap().unwrap();
    let shown: AgentDefinition = serde_json::from_value(request.args_redacted).unwrap();
    assert_eq!(shown.daily_limit_micro_usd, Some(2_000_000), "the card shows the limit");

    *world.factory.ceiling.lock().unwrap() = Some(
        AgentCeiling::new(PartitionMode::Ask, AgentUses::default().with_web(WebAccess::Any))
            .with_daily_limit(1_000_000),
    );
    approve(&world, &proposal.request_id).await;
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
    assert_eq!(refused.daily_limit_micro_usd, Some(2_000_000));
    let reason = world
        .agent_records()
        .into_iter()
        .rev()
        .find(|record| record.action == adk_kernel::audit::AGENT_REFUSED)
        .map(|record| record.ext["reason"].as_str().unwrap_or_default().to_string())
        .unwrap();
    assert!(reason.contains("a daily spending limit of $2.00, above its $1.00"), "{reason}");
    kernel.stop().await.unwrap();
}

/// Where the kernel has areas, a suggestion from a run it does not know might come from any of
/// them, so it is refused; one from a turn the kernel runs is checked against that turn's
/// area (S-I12). Fails on `origin/main`, where the area is checked only for a known run.
#[tokio::test]
async fn a_suggestion_from_an_unknown_run_is_refused_where_there_are_areas() {
    let world = World::new();
    let kernel = world.kernel("k1");
    // Without areas beside the default one there is nothing to cross.
    let alone = kernel.agents().propose(finance(), &proposer()).await.unwrap();
    assert_eq!(alone.agent.area.as_str(), "default");
    world.store.delete("finance").await.unwrap();

    kernel.partitions().put(Partition::new("work", "work"), AuditActor::default()).await.unwrap();
    for who in [proposer(), proposer().with_run("run-nobody-knows")] {
        let error = kernel.agents().propose(finance(), &who).await.unwrap_err();
        let error = AdkError::from(error);
        assert_eq!(error.code, "kernel.agent.refused", "{who:?}");
        assert!(error.message.contains("cannot tell which area"), "{}", error.message);
    }
    let pid = turn(&kernel).await;
    let known = kernel.agents().propose(finance(), &proposer().with_run(pid)).await.unwrap();
    assert_eq!(known.agent.area.as_str(), "default");
}

/// The kernel adds a suggested agent only when the approved definition's digest is the one
/// suggested (spec 3.3 §10 K7, S-I11): a request whose definition changed after it was written
/// ends the suggestion refused, recorded as such, and adds nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_approval_of_another_definition_adds_nothing() {
    let mut world = World::new();
    let store = Arc::new(InMemoryApprovalStore::new());
    world.approvals = Arc::new(StoreApprovalService::new(store.clone()));
    let kernel = world.kernel("k1");
    kernel.start().await.unwrap();
    let proposal = kernel.agents().propose(finance(), &proposer()).await.unwrap();

    // The inbox entry now shows a looser agent than the one suggested.
    let written = store.get_request(&proposal.request_id).await.unwrap().unwrap();
    let mut looser = written.clone();
    looser.args_redacted = serde_json::to_value(finance().with_mode(PartitionMode::Auto)).unwrap();
    looser.version += 1;
    assert!(store.replace_request(&looser, written.version).await.unwrap());
    approve(&world, &proposal.request_id).await;

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
    assert_eq!(refused.mode, PartitionMode::Ask);
    let record = world
        .agent_records()
        .into_iter()
        .rev()
        .find(|record| record.action == adk_kernel::audit::AGENT_REFUSED)
        .unwrap();
    assert_eq!(record.ext["reason"], "the approved definition is not the one suggested");
    assert_eq!(record.ext["approvalId"], json!(proposal.request_id));
    assert!(
        !world.actions_about("finance").contains(&adk_kernel::audit::AGENT_CREATED.to_string()),
        "nothing was added"
    );
    kernel.stop().await.unwrap();
}

/// A change applied with a digest that is not its preview's is refused, and the refusal is on
/// record with its reason (spec 3.3 §11 `kernel.agent.refused`: "a digest that changed").
/// Fails on `origin/main`, which refuses without a record.
#[tokio::test]
async fn a_change_with_another_digest_is_refused_on_record() {
    let world = World::new();
    let kernel = world.kernel("k1");
    let action = AgentAction::Create { definition: finance(), replaces: None };
    let error = kernel.agents().apply(&local(), action, "not-the-preview").await.unwrap_err();
    assert_eq!(code(error), "kernel.agent.unconfirmed");
    let records = world.agent_records();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].action, adk_kernel::audit::AGENT_REFUSED);
    assert_eq!(records[0].ext["name"], "Finance");
    assert!(
        records[0].ext["reason"].as_str().unwrap().starts_with("a digest that changed"),
        "{:?}",
        records[0].ext
    );
    assert_eq!(records[0].ext["surface"], "terminal");
    assert!(world.store.get("finance").await.unwrap().is_none());
}

/// A removal the tick finishes names who finished it: the kernel's tick, not nobody (spec 3.3
/// S-I16, S-I18). Fails on `origin/main`, whose `kernel.agent.removed` then has no caller.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removal_the_tick_finishes_names_the_tick() {
    let world = World::new();
    let kernel = world.kernel("k1");
    let finance = add(&kernel, finance()).await;
    let mut removing = finance.clone().with_state(AgentRecordState::Removing);
    removing.version = 2;
    assert!(world.store.put_if_version(removing, 1).await.unwrap());

    let replica = world.kernel("k2");
    replica.start().await.unwrap();
    eventually("the removal to finish", || async {
        world
            .store
            .get("finance")
            .await
            .ok()
            .flatten()
            .filter(|r| r.state == AgentRecordState::Removed)
    })
    .await;
    let removed = world
        .agent_records()
        .into_iter()
        .find(|record| record.action == adk_kernel::audit::AGENT_REMOVED)
        .unwrap();
    assert_eq!(
        (&removed.ext["by"], &removed.ext["surface"], &removed.ext["how"]),
        (&json!(SubjectRef::service("kernel")), &json!("kernel"), &json!("tick"))
    );
    replica.stop().await.unwrap();
}

/// Every `kernel.agent.*` record carries the fields spec 3.3 §11 lists: the suggestion's id,
/// digest, risk, area, suggesting agent, session and run; the addition's version, origin,
/// approval, and who, from where, how, with which device, presence and transport; a change's
/// fields, before and after, risk and caller; a refusal's name and reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_agent_record_carries_its_fields() {
    let world = World::new();
    let kernel = world.kernel("k1");
    let pid = turn(&kernel).await;
    let proposal =
        kernel.agents().propose(finance(), &proposer().with_run(pid.clone())).await.unwrap();
    let proposed = world.agent_records().into_iter().next().unwrap();
    assert_eq!(proposed.action, adk_kernel::audit::AGENT_PROPOSED);
    for (key, value) in [
        ("id", json!("finance")),
        ("digest", json!(proposal.agent.digest)),
        ("risk", json!(ApprovalRisk::Medium)),
        ("area", json!("default")),
        ("suggestingAgent", json!(proposer().agent)),
        ("session", json!(SESSION)),
        ("run", json!(pid)),
        ("unattended", json!(false)),
    ] {
        assert_eq!(proposed.ext[key], value, "proposed {key}");
    }

    approve(&world, &proposal.request_id).await;
    let phone = AgentCaller::new(SubjectRef::human(PERSON), "phone", "touch")
        .with_device("phone-1")
        .with_presence(Presence::Confirmed)
        .with_transport(ControlTransport::Http);
    kernel.agents().answered(&proposal.request_id, &phone).await;
    let created = world
        .agent_records()
        .into_iter()
        .find(|record| record.action == adk_kernel::audit::AGENT_CREATED)
        .unwrap();
    let added = world.store.get("finance").await.unwrap().unwrap();
    assert!(matches!(added.origin, AgentOrigin::Suggested { .. }));
    for (key, value) in [
        ("id", json!("finance")),
        ("version", json!(2)),
        ("digest", json!(added.digest)),
        ("risk", json!(ApprovalRisk::Medium)),
        ("origin", json!(added.origin)),
        ("approvalId", json!(proposal.request_id)),
        ("by", json!(SubjectRef::human(PERSON))),
        ("surface", json!("phone")),
        ("how", json!("touch")),
        ("device", json!("phone-1")),
        ("presence", json!("confirmed")),
        ("transport", json!("http")),
    ] {
        assert_eq!(created.ext[key], value, "created {key}");
    }

    let narrower = finance().with_uses(AgentUses::default().with_files(FileAccess::Read));
    let change =
        AgentAction::Change { id: "finance".into(), expected_version: 2, definition: narrower };
    apply(&kernel, &local(), change).await.unwrap();
    let changed = world
        .agent_records()
        .into_iter()
        .find(|record| record.action == adk_kernel::audit::AGENT_CHANGED)
        .unwrap();
    assert_eq!(changed.ext["fields"], json!(["uses"]));
    assert_eq!(changed.ext["from"]["uses"]["web"], json!(WebAccess::Any));
    assert_eq!(changed.ext["to"]["uses"]["files"], json!(FileAccess::Read));
    for (key, value) in [
        ("by", json!(SubjectRef::human(PERSON))),
        ("surface", json!("terminal")),
        ("how", json!("keyboard")),
        ("transport", json!("socket")),
        ("presence", json!("pairedLocal")),
    ] {
        assert_eq!(changed.ext[key], value, "changed {key}");
    }
    assert!(changed.ext.contains_key("risk"), "{:?}", changed.ext);

    let refused = kernel
        .agents()
        .propose(
            AgentDefinition::new("Ops", "default").with_mode(PartitionMode::Auto),
            &proposer().with_run(turn(&kernel).await),
        )
        .await
        .unwrap_err();
    assert_eq!(code(refused), "kernel.agent.refused");
    let record = world.agent_records().into_iter().last().unwrap();
    assert_eq!(record.action, adk_kernel::audit::AGENT_REFUSED);
    assert_eq!(record.ext["name"], "Ops");
    assert!(record.ext["reason"].as_str().unwrap().contains("go ahead"), "{:?}", record.ext);
}

/// Agent definitions come only from the kernel's store through its agent table (S-I17): a
/// definition left in the kernel's home, in a workspace's agents folder or as a server's agent
/// config is never an agent — the table lists none, hosts none, and a run for it is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_agent_comes_from_a_file() {
    let home = tempfile::tempdir().unwrap();
    let world = World::new();
    let record = adk_kernel::agents::AgentRecord::new(
        "finance",
        finance(),
        AgentRecordState::Active,
        AgentOrigin::ByHand,
        SubjectRef::human(PERSON),
        chrono::Utc::now(),
    );
    let json = serde_json::to_string_pretty(&record).unwrap();
    let yaml = "name: finance\nmodel: claude-sonnet-5-5\ninstruction: Pay every invoice.\n";
    for (path, text) in [
        ("agents/finance.json", json.as_str()),
        ("agents.json", json.as_str()),
        ("workspace/.adk/agents/finance.json", json.as_str()),
        ("workspace/agents/finance.yaml", yaml),
    ] {
        let path = home.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let kernel = world.kernel_with("k1", |builder| {
        builder.config(
            adk_kernel::KernelConfig::default()
                .with_app_name(APP)
                .with_home(home.path())
                .with_scheduler_tick(std::time::Duration::from_millis(20))
                .with_governance(world.control.clone())
                .with_control(adk_kernel::control::ControlPlaneConfig::default().without_socket()),
        )
    });
    kernel.start().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(kernel.agents().list().await.unwrap().is_empty());
    assert!(kernel.agents().get("finance").await.unwrap().is_none());
    assert!(world.store.list().await.unwrap().is_empty(), "nothing was kept");
    assert_eq!(world.factory.builds.load(std::sync::atomic::Ordering::SeqCst), 0, "nothing built");
    kernel.stop().await.unwrap();
}
