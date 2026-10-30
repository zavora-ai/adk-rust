//! K7, partitions (spec 3.2 §5, §7.8, §13 Q3, K-I15) on `ScriptedLlm`: one harness per
//! partition over one fact store. A work agent's read of a home fact is denied at the
//! policy; the coordinator's crossing is held under a non-waivable approval, an approved read
//! holds for the rest of its run and nothing beyond it, a write is asked each time, and every
//! crossing is audited as `kernel.partition.crossing`. Hosted by a kernel, the coordinator's
//! process parks on the crossing and its identical read after the decision is not asked again.

#![cfg(all(feature = "kernel", feature = "memory"))]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    DurableRunStore, InMemoryRunStore, NewRun, RunConfig, RunState, ToolApproval,
    ToolConfirmationRequest,
};
use adk_harness::{
    FactKind, FactScope, FactSource, FactStatus, FactStore, Harness, HarnessAgent, HarnessDecision,
    InMemoryFactStore, LongTermMemoryConfig, MemoryFact, PARTITION_MEMORY_REMEMBER,
    PARTITION_MEMORY_SEARCH, PromptAnswer, Provenance,
};
use adk_kernel::audit::PARTITION_CROSSING;
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::{
    CrossPartition, CrossingFilter, CrossingOutcome, CrossingRun, InMemoryProcessStore,
    KernelConfig, Partition, PartitionId, Partitions, ProcessFilter, ProcessKind, ProcessSpec,
    ProcessStore, QueueClass,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_server::background::DurableWorkerConfig;
use adk_session::{InMemorySessionService, SessionService};
use common::{APP, ScriptedPrompter, collect, no_user_file, outcome, response_of, responses, user};
use serde_json::{Value, json};

fn search(partition: &str, query: &str) -> ScriptedResponse {
    ScriptedResponse::function_call(
        PARTITION_MEMORY_SEARCH,
        json!({ "partition": partition, "query": query }),
    )
}

fn remember(partition: &str, text: &str) -> ScriptedResponse {
    ScriptedResponse::function_call(
        PARTITION_MEMORY_REMEMBER,
        json!({ "partition": partition, "text": text, "kind": "decision" }),
    )
}

fn text(text: &str) -> ScriptedResponse {
    ScriptedResponse::text(text)
}

/// One user's home and work, both open to the coordinator, over one fact store, with the
/// partition records written to their own audit trail.
struct World {
    facts: Arc<InMemoryFactStore>,
    partitions: Partitions,
    crossings: InMemoryAuditRecorder,
    sessions: Arc<dyn SessionService>,
    dirs: Vec<tempfile::TempDir>,
}

impl World {
    async fn new() -> Self {
        let crossings = InMemoryAuditRecorder::new();
        let partitions =
            Partitions::in_memory().with_audit(AuditTrail::new(Arc::new(crossings.clone())));
        for id in ["home", "work"] {
            let partition = Partition::new(id, format!("{id}-agents"))
                .with_cross_partition(CrossPartition::CoordinatorWithApproval);
            partitions.put(partition, AuditActor::default()).await.unwrap();
        }
        let facts = Arc::new(InMemoryFactStore::new());
        for (project, fact) in
            [("home", "dentist appointment on tuesday"), ("work", "standup is at nine")]
        {
            let scope = FactScope::new(APP, "alice", Some(project.into()));
            let provenance = Provenance::new(FactSource::UserStated, "seed", "seed");
            facts
                .put(MemoryFact::new(
                    scope,
                    FactKind::ProjectFact,
                    fact,
                    FactStatus::Known,
                    provenance,
                ))
                .await
                .unwrap();
        }
        Self {
            facts,
            partitions,
            crossings,
            sessions: Arc::new(InMemorySessionService::new()),
            dirs: Vec::new(),
        }
    }

    /// The harness of partition `id` over `script`; the coordinator when `coordinator`.
    async fn harness(
        &mut self,
        id: &str,
        coordinator: bool,
        script: Vec<ScriptedResponse>,
        configure: impl FnOnce(adk_harness::HarnessAgentBuilder) -> adk_harness::HarnessAgentBuilder,
    ) -> (Harness, Arc<ScriptedLlm>) {
        let dir = tempfile::tempdir().unwrap();
        let model = Arc::new(ScriptedLlm::new("scripted", script));
        let partition = self.partitions.get(&PartitionId::new(id)).await.unwrap().unwrap();
        let mut builder = HarnessAgent::builder("harness")
            .model(model.clone())
            .workspace(adk_devtools::Workspace::new(dir.path()))
            .project_instructions(no_user_file())
            .without_compaction()
            .without_default_members()
            .long_term_memory(LongTermMemoryConfig {
                distill: false,
                ..LongTermMemoryConfig::default()
            })
            .fact_store(self.facts.clone())
            .partition(partition, self.partitions.clone());
        if coordinator {
            builder = builder.partition_coordinator();
        }
        let harness =
            configure(builder).build().unwrap().into_harness(APP, self.sessions.clone()).unwrap();
        self.dirs.push(dir);
        (harness, model)
    }

    fn crossing_records(&self) -> Vec<AuditRecordDraft> {
        self.crossings
            .records()
            .into_iter()
            .filter(|draft| draft.action == PARTITION_CROSSING)
            .collect()
    }

    fn outcomes(&self) -> Vec<String> {
        self.crossing_records()
            .iter()
            .map(|draft| draft.ext["outcome"].as_str().unwrap_or_default().to_string())
            .collect()
    }
}

async fn send(harness: &Harness, session: &str, message: &str) -> Vec<adk_core::Event> {
    let user = user("alice");
    let session = harness
        .open_session(&user, Some(&adk_core::SessionId::new(session).unwrap()))
        .await
        .unwrap();
    collect(harness.send(&user, &session, message).await.unwrap()).await
}

/// K7: a work agent asks for a home fact and is denied at the policy, without a prompt.
#[tokio::test]
async fn a_work_agents_read_of_a_home_fact_is_denied_at_the_policy() {
    let mut world = World::new().await;
    let prompter = ScriptedPrompter::new();
    let (work, _) = world
        .harness("work", false, vec![search("home", "dentist"), text("I cannot see that.")], {
            let prompter = prompter.clone();
            move |builder| builder.prompter(prompter)
        })
        .await;
    let events = send(&work, "w1", "when is my dentist appointment?").await;
    let response = response_of(&events, PARTITION_MEMORY_SEARCH);
    let error = response["error"].as_str().unwrap_or_default();
    assert!(error.contains("partition crossing denied"), "{response}");
    assert!(!response.to_string().contains("tuesday"), "the home fact never reaches work");
    assert_eq!(prompter.count(), 0, "a denial asks no one");
    let records = world.crossing_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].ext["partition"], "work");
    assert_eq!(records[0].ext["to"], "home");
    assert_eq!(records[0].ext["outcome"], "denied");
    // The work agent acts with its partition's class (spec 3.1 §7.14).
    assert!(records[0].ext["by"].as_str().unwrap().starts_with("urn:adk:agent:"));
}

/// K-I15: a partition's harness marks each session it runs as its partition's, and another
/// partition's harness refuses that session, so neither reads the other's transcript.
#[tokio::test]
async fn a_partitions_session_is_marked_and_another_partitions_harness_refuses_it() {
    use futures::StreamExt as _;

    let mut world = World::new().await;
    let (home, _) = world.harness("home", false, vec![text("Noted.")], |builder| builder).await;
    let (work, model) = world.harness("work", false, vec![text("unused")], |builder| builder).await;
    send(&home, "shared", "my dentist appointment is on tuesday").await;
    let state = |session: &str| {
        let sessions = world.sessions.clone();
        let session = session.to_string();
        async move {
            sessions
                .get(adk_session::GetRequest {
                    app_name: APP.into(),
                    user_id: "alice".into(),
                    session_id: session,
                    num_recent_events: None,
                    after: None,
                })
                .await
                .unwrap()
                .state()
                .get(adk_harness::PARTITION_STATE_KEY)
        }
    };
    assert_eq!(state("shared").await, Some(json!("home")));

    let user = user("alice");
    let session =
        work.open_session(&user, Some(&adk_core::SessionId::new("shared").unwrap())).await.unwrap();
    let refused = match work.send(&user, &session, "when is my dentist appointment?").await {
        Err(error) => error,
        Ok(stream) => {
            let events: Vec<_> = stream.collect().await;
            events
                .into_iter()
                .find_map(Result::err)
                .expect("the work harness refuses home's session")
        }
    };
    assert_eq!(refused.code, adk_harness::PARTITION_SESSION_CODE, "{refused}");
    assert!(model.requests().is_empty(), "no model call reads the session");
    assert_eq!(state("shared").await, Some(json!("home")), "the mark is unchanged");

    send(&work, "w-own", "hello").await;
    assert_eq!(state("w-own").await, Some(json!("work")));
}

/// K-I15: a partition's consolidation tick works on its partition's facts only; a stale
/// user-wide assumption, which every partition could read on an unpartitioned harness, is
/// left alone, as is the other partition's.
#[tokio::test]
async fn a_partitions_consolidation_tick_touches_only_its_own_facts() {
    let mut world = World::new().await;
    let stale = |project: Option<&str>, text: &str| {
        let mut fact = MemoryFact::new(
            FactScope::new(APP, "alice", project.map(str::to_string)),
            FactKind::ProjectFact,
            text,
            FactStatus::Assumed,
            Provenance::new(FactSource::AgentInferred, "s0", "inv-0"),
        );
        fact.valid_from = chrono::Utc::now() - chrono::Duration::days(100);
        fact.confidence = Some(0.3);
        fact
    };
    let mut seeded = Vec::new();
    for (project, text) in [
        (Some("home"), "the spare key is under the mat"),
        (None, "the user prefers tea"),
        (Some("work"), "the build cache lives in /tmp/build"),
    ] {
        seeded.push(world.facts.put(stale(project, text)).await.unwrap());
    }
    // Known facts no pass touches, so one expiry stays within the loss cap.
    for i in 0..4 {
        let mut filler = stale(Some("home"), &format!("filler fact {i}"));
        filler.status = FactStatus::Known;
        world.facts.put(filler).await.unwrap();
    }
    let (home, _) = world.harness("home", false, Vec::new(), |builder| builder).await;

    let record = home
        .consolidate_memory(
            &adk_core::UserId::new("alice").unwrap(),
            adk_harness::ConsolidationRequest::new(),
        )
        .await
        .unwrap();

    assert_eq!(record.scopes, vec![FactScope::new(APP, "alice", Some("home".into()))]);
    assert_eq!(record.refused, None);
    let status = |fact: &MemoryFact| {
        let facts = world.facts.clone();
        let id = fact.id.clone();
        async move { facts.get(&id).await.unwrap().unwrap().status }
    };
    assert_eq!(status(&seeded[0]).await, FactStatus::Forgotten, "home's own stale fact");
    assert_eq!(status(&seeded[1]).await, FactStatus::Assumed, "the user-wide fact");
    assert_eq!(status(&seeded[2]).await, FactStatus::Assumed, "work's fact");
}

/// Spec 3.1 §7.14, 3.2 §7.8: every agent of a partition's harness acts with the partition's
/// class, through the runner's identity source, so a `PolicyRule::for_class` rule applies to
/// one partition's agents and not another's.
#[tokio::test]
async fn class_rules_apply_to_the_partitions_agents() {
    let mut world = World::new().await;
    let policy = || -> Arc<dyn adk_core::ToolPolicy> {
        Arc::new(
            adk_core::DeclarativePolicy::builder()
                .rule(
                    adk_core::PolicyRule::deny("memory_search", "work agents do not search memory")
                        .for_class("work-agents"),
                )
                .default_decision(adk_core::PolicyDecision::Allow)
                .build(),
        )
    };
    let script = |answer: &str| {
        vec![
            ScriptedResponse::function_call("memory_search", json!({ "query": "standup" })),
            text(answer),
        ]
    };
    let (work, _) =
        world.harness("work", false, script("no"), |b| b.operator_policy(policy())).await;
    let (home, _) =
        world.harness("home", true, script("ok"), |b| b.operator_policy(policy())).await;
    let denied = response_of(&send(&work, "w1", "search").await, "memory_search");
    assert!(
        denied["error"].as_str().unwrap_or_default().contains("work agents do not search"),
        "{denied}"
    );
    let allowed = response_of(&send(&home, "h1", "search").await, "memory_search");
    assert_eq!(outcome(&allowed), common::Outcome::Executed, "{allowed}");
    assert_eq!(world.outcomes(), Vec::<String>::new(), "a partition's own memory is no crossing");
}

/// §13 Q3: the coordinator's second identical read in the same run is not asked again, a
/// write in the same run is, and the next run asks again.
#[tokio::test]
async fn an_approved_read_holds_for_the_run_a_write_is_asked_and_the_next_run_asks_again() {
    let mut world = World::new().await;
    let prompter = ScriptedPrompter::new();
    let script = vec![
        search("work", "standup"),
        search("work", "standup"),
        remember("work", "the retro moves to friday"),
        text("Standup is at nine; I noted the retro."),
        search("work", "standup"),
        text("Still nine."),
    ];
    let (home, model) = world
        .harness("home", true, script, {
            let prompter = prompter.clone();
            move |builder| builder.prompter(prompter)
        })
        .await;
    for _ in 0..3 {
        prompter.then(|_| PromptAnswer::ApproveOnce);
    }

    let events = send(&home, "h1", "when is standup? and note the retro move").await;
    let searches: Vec<Value> = responses(&events)
        .into_iter()
        .filter(|(name, _)| name == PARTITION_MEMORY_SEARCH)
        .map(|(_, response)| response)
        .collect();
    assert_eq!(searches.len(), 2);
    for found in &searches {
        assert_eq!(outcome(found), common::Outcome::Executed, "{found}");
        assert!(found.to_string().contains("standup is at nine"), "{found}");
    }
    assert_eq!(
        outcome(&response_of(&events, PARTITION_MEMORY_REMEMBER)),
        common::Outcome::Executed
    );
    assert_eq!(prompter.count(), 2, "the read once, the write once; the second read not again");
    let prompts = prompter.prompts.lock().unwrap().clone();
    assert!(
        prompts[0].rendered.title.starts_with("Partition crossing into work › memory"),
        "{}",
        prompts[0].rendered.title
    );
    assert!(
        prompts.iter().all(|prompt| prompt.suggestions.is_empty()),
        "no rule answers a crossing"
    );

    // The write landed in work's scope.
    let work_facts = world
        .facts
        .list(adk_harness::FactFilter::exactly(&FactScope::new(APP, "alice", Some("work".into()))))
        .await
        .unwrap();
    assert!(work_facts.iter().any(|fact| fact.text.contains("retro moves to friday")));

    // The turn ran outside a durable run, so its grant was keyed by its invocation and
    // dropped when it ended.
    let ledger = world.partitions.crossings(&CrossingFilter::default()).await.unwrap();
    let invocation = ledger[0].run.clone();
    assert!(ledger.iter().all(|record| record.run == invocation), "one invocation: {ledger:?}");
    let grants = world.partitions.grants(&CrossingRun::Invocation(invocation.clone())).await;
    assert!(grants.unwrap().is_empty(), "the ended invocation holds no grant");

    // The next run asks again.
    send(&home, "h1", "and standup again?").await;
    assert_eq!(prompter.count(), 3);
    let next = world.partitions.crossings(&CrossingFilter::default()).await.unwrap();
    assert_ne!(next.last().unwrap().run, invocation, "a new run");
    assert_eq!(
        world.outcomes(),
        ["held", "approved", "runGrant", "held", "approved", "held", "approved"]
    );
    assert!(model.requests().len() >= 6);
}

/// K-I15: a crossing is non-waivable. Auto mode still asks, the prompt offers no rule, and
/// a run-wide approval a caller passes for it is removed.
#[tokio::test]
async fn a_crossing_is_asked_in_auto_mode_and_a_run_wide_approval_of_it_is_stripped() {
    let mut world = World::new().await;
    let prompter = ScriptedPrompter::new();
    let (home, _) = world
        .harness("home", true, vec![search("work", "standup"), text("denied, fine")], {
            let prompter = prompter.clone();
            move |builder| builder.prompter(prompter).mode(adk_harness::HarnessMode::Auto)
        })
        .await;
    prompter.deny();
    let events = send(&home, "h1", "standup?").await;
    assert_eq!(prompter.count(), 1, "Auto still asks for a crossing");
    assert_eq!(outcome(&response_of(&events, PARTITION_MEMORY_SEARCH)), common::Outcome::Denied);

    let crossing = ToolConfirmationRequest::new(
        PARTITION_MEMORY_SEARCH,
        json!({ "partition": "work", "query": "standup" }),
    );
    let mut config = RunConfig::default();
    config.tool_approvals.insert(crossing.fingerprint(), ToolApproval::approve());
    home.agent().runtime().layer().apply(&mut config);
    assert!(config.tool_approvals.is_empty(), "no run-wide approval answers a crossing");
}

/// Answers every crossing "deny and block crossings to *X* from this partition", through
/// `prompt_crossing`; any other prompt is denied.
#[derive(Debug, Default)]
struct BlockingPrompter {
    seen: std::sync::Mutex<Vec<adk_harness::CrossingPrompt>>,
}

#[async_trait::async_trait]
impl adk_harness::ApprovalPrompter for BlockingPrompter {
    async fn prompt(&self, _prompt: adk_harness::ApprovalPrompt) -> adk_core::Result<PromptAnswer> {
        Ok(PromptAnswer::Deny { message: None })
    }

    async fn prompt_crossing(
        &self,
        prompt: adk_harness::CrossingPrompt,
    ) -> adk_core::Result<adk_harness::CrossingAnswer> {
        self.seen.lock().unwrap().push(prompt);
        Ok(adk_harness::CrossingAnswer::DenyAndBlock)
    }
}

/// "deny and block crossings to work from this partition" blocks the next crossing at the
/// policy, without a prompt.
#[tokio::test]
async fn deny_and_block_refuses_later_crossings_without_asking() {
    let mut world = World::new().await;
    let prompter = Arc::new(BlockingPrompter::default());
    let script = vec![search("work", "standup"), search("work", "salary"), text("blocked")];
    let (home, _) = world
        .harness("home", true, script, {
            let prompter = prompter.clone();
            move |builder| builder.prompter(prompter)
        })
        .await;
    let events = send(&home, "h1", "standup and salary?").await;
    let seen = prompter.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "the block answers the second crossing");
    assert_eq!(
        seen[0].answers,
        [
            "allow this read of work › memory for this run".to_string(),
            "deny".to_string(),
            "deny and block crossings to work from this partition".to_string(),
        ]
    );
    let home_record = world.partitions.get(&PartitionId::new("home")).await.unwrap().unwrap();
    assert!(home_record.blocked_crossings.contains(&PartitionId::new("work")));
    let answers: Vec<String> = responses(&events)
        .into_iter()
        .filter(|(name, _)| name == PARTITION_MEMORY_SEARCH)
        .map(|(_, response)| response["error"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(answers.len(), 2, "{answers:?}");
    assert!(answers[0].contains("blocked"), "{answers:?}");
    assert!(answers[1].contains("blocked"), "{answers:?}");
    assert!(world.outcomes().ends_with(&["denied".to_string()]), "{:?}", world.outcomes());
}

/// A plain prompter's answer maps onto a crossing's three answers.
#[test]
fn a_plain_prompters_answers_map_onto_the_crossing_answers() {
    use adk_harness::CrossingAnswer;
    assert_eq!(
        CrossingAnswer::from_prompt_answer(PromptAnswer::ApproveOnce),
        CrossingAnswer::Allow
    );
    assert!(matches!(
        CrossingAnswer::from_prompt_answer(PromptAnswer::ApproveWithArgs(json!({}))),
        CrossingAnswer::Deny { .. }
    ));
    assert_eq!(
        CrossingAnswer::from_prompt_answer(PromptAnswer::Deny { message: None }),
        CrossingAnswer::Deny { message: None }
    );
    assert_eq!(
        CrossingAnswer::from_prompt_answer(PromptAnswer::DenyAllRemaining),
        CrossingAnswer::Deny { message: None }
    );
}

async fn eventually<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let started = tokio::time::Instant::now();
    while started.elapsed() <= Duration::from_secs(10) {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn in_state(runs: &Arc<dyn DurableRunStore>, pid: &str, state: RunState) {
    eventually(&format!("{pid} to be {state:?}"), || async {
        runs.get(pid).await.ok().flatten().filter(|run| run.state == state).map(|_| ())
    })
    .await;
}

fn turn(target: &str, session: &str, message: &str) -> NewRun {
    NewRun::new(QueueClass::INTERACTIVE, APP, "alice", target, json!({ "text": message }))
        .with_session_id(session)
}

/// K7 on a kernel hosting both partitions: the work agent's crossing is denied, the
/// coordinator's process parks on its crossing until a person decides in the inbox, its
/// identical read after the decision runs without asking again, and the next process asks
/// again; every crossing is in the audit, and `ps` shows each process in its partition.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn k7_the_kernel_hosts_both_partitions_and_the_coordinators_crossing_parks_for_approval() {
    let mut world = World::new().await;
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let (home, _) = world
        .harness(
            "home",
            true,
            vec![
                search("work", "standup"),
                search("work", "standup"),
                text("Standup is at nine."),
                search("work", "standup"),
                text("asked again"),
            ],
            |builder| builder,
        )
        .await;
    let (work, _) = world
        .harness("work", false, vec![search("home", "dentist"), text("not mine to see")], |b| b)
        .await;
    let home = home.with_run_store(Arc::clone(&runs));
    let work = work.with_run_store(Arc::clone(&runs));
    let processes = Arc::new(InMemoryProcessStore::new());
    let kernel = work
        .host_partition(home.kernel())
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_worker(
                    DurableWorkerConfig::default()
                        .with_worker_id("k1")
                        .with_lease_ttl(Duration::from_millis(600)),
                )
                .with_scheduler_tick(Duration::from_millis(50))
                .with_control(ControlPlaneConfig::default().without_socket()),
        )
        .process_store(Arc::clone(&processes) as Arc<dyn ProcessStore>)
        .build()
        .unwrap();
    kernel.start().await.unwrap();
    let home_target = adk_harness::partition_target(APP, &PartitionId::new("home"));
    let work_target = adk_harness::partition_target(APP, &PartitionId::new("work"));

    // The work agent's read of a home fact: denied at the policy, the run completes.
    let asked = kernel
        .processes()
        .spawn(turn(&work_target, "w1", "dentist?"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap();
    assert_eq!(asked.meta.partition, PartitionId::new("work"));
    in_state(&runs, &asked.meta.pid, RunState::Completed).await;
    assert_eq!(world.outcomes(), ["denied"]);

    // The coordinator's crossing parks its process until a person decides.
    let coordinator = kernel
        .processes()
        .spawn(turn(&home_target, "h1", "standup?"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap();
    let pid = coordinator.meta.pid.clone();
    assert_eq!(coordinator.meta.partition, PartitionId::new("home"));
    in_state(&runs, &pid, RunState::Waiting).await;
    let alice = user("alice");
    let h1 = adk_core::SessionId::new("h1").unwrap();
    let pending = home.pending_approvals(&alice, &h1).await.unwrap();
    assert_eq!(pending.len(), 1, "one crossing, one approval");
    home.decide(&alice, &h1, &pending[0].fingerprint, HarnessDecision::Approve).await.unwrap();
    in_state(&runs, &pid, RunState::Completed).await;
    assert!(
        home.pending_approvals(&alice, &h1).await.unwrap().is_empty(),
        "the identical read was not asked"
    );
    let ledger = world
        .partitions
        .crossings(&CrossingFilter::default().run(&pid))
        .await
        .unwrap()
        .into_iter()
        .map(|record| record.outcome)
        .collect::<Vec<_>>();
    assert_eq!(
        ledger,
        [CrossingOutcome::Held, CrossingOutcome::Approved, CrossingOutcome::RunGrant]
    );
    // The finished run holds no grant: the worker released it.
    let durable = CrossingRun::Durable(pid.clone());
    eventually("the finished run's grants to be released", || async {
        world.partitions.grants(&durable).await.unwrap().is_empty().then_some(())
    })
    .await;

    // The next process asks again.
    let next = kernel
        .processes()
        .spawn(
            turn(&home_target, "h2", "standup again?"),
            ProcessSpec::new(ProcessKind::Interactive),
        )
        .await
        .unwrap();
    in_state(&runs, &next.meta.pid, RunState::Waiting).await;
    let h2 = adk_core::SessionId::new("h2").unwrap();
    assert_eq!(home.pending_approvals(&alice, &h2).await.unwrap().len(), 1);

    // Each process is listed in its partition.
    for (partition, expected) in [("home", 2), ("work", 1)] {
        let listed =
            kernel.processes().ps(ProcessFilter::default().partition(partition)).await.unwrap();
        assert_eq!(listed.len(), expected, "{partition}: {listed:?}");
    }
    assert!(
        world.crossing_records().iter().all(|draft| draft.ext.contains_key("fingerprintDigest"))
    );
    kernel.stop().await.unwrap();
}

/// Spec 3.2 §7.5, §7.8: a partition's follow-up is on its partition's agenda and, once
/// approved, runs as a process of that partition on its harness — proposed by a kernel
/// process, or by a run outside the kernel.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_partitions_followup_is_on_its_agenda_and_runs_its_partitions_harness() {
    let mut world = World::new().await;
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let propose = |title: &str| {
        ScriptedResponse::function_call(
            "propose_followup",
            json!({ "title": title, "rationale": "the release lands tonight" }),
        )
    };
    let (home, _) = world.harness("home", true, Vec::new(), |builder| builder).await;
    let (work, _) = world
        .harness(
            "work",
            false,
            vec![
                propose("Check the release notes"),
                text("Proposed."),
                text("The notes read well."),
                propose("Check the changelog"),
                text("Proposed."),
                text("The changelog reads well."),
            ],
            |builder| builder,
        )
        .await;
    let home = home.with_run_store(Arc::clone(&runs));
    let work = work.with_run_store(Arc::clone(&runs));
    let kernel = work
        .host_partition(home.kernel())
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_scheduler_tick(Duration::from_millis(50))
                .with_control(ControlPlaneConfig::default().without_socket()),
        )
        .build()
        .unwrap();
    kernel.start().await.unwrap();
    let work_target = adk_harness::partition_target(APP, &PartitionId::new("work"));
    let alice = adk_core::principal::SubjectRef::human("alice");

    let followup_of = |title: &'static str| {
        let kernel = kernel.clone();
        async move {
            let items = kernel
                .agenda()
                .list(&adk_kernel::AgendaFilter::default().partition("work"))
                .await
                .unwrap();
            items.into_iter().find(|item| item.title == title)
        }
    };
    let ran = |id: String| {
        let kernel = kernel.clone();
        async move {
            eventually("the follow-up's process to complete", || async {
                let processes = kernel.processes().ps(ProcessFilter::default()).await.unwrap();
                processes.into_iter().find(|process| {
                    process.meta.agenda_item.as_deref() == Some(id.as_str())
                        && process.run.state == RunState::Completed
                })
            })
            .await
        }
    };

    // Proposed by a kernel process of work.
    let turn = kernel
        .processes()
        .spawn(turn(&work_target, "w1", "ship it"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap();
    in_state(&runs, &turn.meta.pid, RunState::Completed).await;
    let item = followup_of("Check the release notes").await.expect("on work's agenda");
    kernel.agenda().approve(&item.id, &alice).await.unwrap();
    let process = ran(item.id.clone()).await;
    assert_eq!(process.meta.partition, PartitionId::new("work"));
    assert_eq!(process.run.target, work_target);
    assert_eq!(process.meta.parent.as_deref(), Some(turn.meta.pid.as_str()));

    // Proposed by a run outside the kernel: the follow-up still runs work's harness.
    send(&work, "w2", "ship the changelog").await;
    let item = followup_of("Check the changelog").await.expect("on work's agenda");
    kernel.agenda().approve(&item.id, &alice).await.unwrap();
    let process = ran(item.id.clone()).await;
    assert_eq!(process.meta.partition, PartitionId::new("work"));
    assert_eq!(process.run.target, work_target, "not the coordinator's target");
    assert!(
        kernel
            .agenda()
            .list(&adk_kernel::AgendaFilter::default().partition("home"))
            .await
            .unwrap()
            .is_empty(),
        "nothing landed on home's agenda"
    );
    kernel.stop().await.unwrap();
}

/// An observation of `partition` the baseline pass would put to alice (spec 3.2 §7.10).
fn observation(id: &str, partition: &str, statement: &str) -> adk_kernel::AgendaItem {
    adk_kernel::AgendaItem::new(
        id,
        adk_core::SubjectRef::human("alice"),
        "Your routine has changed. Would you like to review what changed?",
        statement,
        adk_kernel::AgendaOrigin::Observation { id: id.into() },
        chrono::Utc::now(),
    )
    .with_partition(partition)
}

/// Spec 3.2 §7.8, §7.10, K-I15: under a kernel with a ledger, each partition's harness
/// writes its rows in its partition, not `default`; each partition's observation is put to
/// its own harness only and, once confirmed, remembered in that partition's memory scope,
/// where the other partition's facts never show it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rows_and_observations_stay_in_their_partition() {
    use adk_kernel::ObservationAudience as _;
    use adk_kernel::proc::{InMemoryLedgerStore, Ledger, LedgerKey, LedgerQuery, LedgerStore};

    let mut world = World::new().await;
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let (home, _) = world.harness("home", true, vec![text("home ok")], |b| b).await;
    let (work, _) = world.harness("work", false, vec![text("work ok")], |b| b).await;
    let home = home.with_run_store(Arc::clone(&runs));
    let work = work.with_run_store(Arc::clone(&runs));
    let alice = user("alice");
    let (home_audience, work_audience) =
        (home.observation_audience(&alice).unwrap(), work.observation_audience(&alice).unwrap());
    let store = Arc::new(InMemoryLedgerStore::new());
    let kernel = work
        .host_partition(home.kernel())
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_scheduler_tick(Duration::from_millis(50))
                .with_control(ControlPlaneConfig::default().without_socket()),
        )
        .ledger(Ledger::new(store.clone(), LedgerKey::from_bytes([3; 32])))
        .observation_audience("home", Arc::new(home_audience.clone()))
        .observation_audience("work", Arc::new(work_audience.clone()))
        .build()
        .unwrap();
    kernel.start().await.unwrap();

    // A work session's rows are work's, a home session's home's.
    for (partition, session) in [("work", "w1"), ("home", "h1")] {
        let target = adk_harness::partition_target(APP, &PartitionId::new(partition));
        let process = kernel
            .processes()
            .spawn(turn(&target, session, "hello"), ProcessSpec::new(ProcessKind::Interactive))
            .await
            .unwrap();
        in_state(&runs, &process.meta.pid, RunState::Completed).await;
    }
    let harness_agent = format!("urn:adk:agent:{APP}/harness");
    let intents = eventually("both partitions' intent rows", || async {
        let rows = store.list(&LedgerQuery::default()).await.unwrap();
        let intents: Vec<String> = rows
            .iter()
            .filter(|row| row.kind == "intent" && row.agent == harness_agent)
            .map(|row| row.partition.to_string())
            .collect();
        (intents.len() == 2).then_some(intents)
    })
    .await;
    let mut intents = intents;
    intents.sort();
    assert_eq!(intents, ["home", "work"], "never `default`");

    // An observation is put to its own partition's harness only.
    let work_id = PartitionId::new("work");
    assert!(home_audience.audience(&work_id).await.unwrap().is_none());
    assert!(work_audience.audience(&work_id).await.unwrap().is_some());
    let work_item = observation("obs-work", "work", "Standups have moved later.");
    let refused = home_audience.confirm(&work_item, &adk_core::SubjectRef::human("alice")).await;
    assert!(refused.is_err(), "home's harness does not remember work's observation");

    // Confirmed: remembered in the partition's own scope, invisible to the other.
    let agenda = kernel.agenda();
    for (item, mode) in [
        (work_item, adk_kernel::PartitionMode::Ask),
        (
            observation("obs-home", "home", "Evenings have got busier."),
            adk_kernel::PartitionMode::Ask,
        ),
    ] {
        let id = item.id.clone();
        agenda.propose(item, mode).await.unwrap();
        let confirmed = agenda.approve(&id, &adk_core::SubjectRef::human("alice")).await.unwrap();
        assert!(confirmed.answer.unwrap().confirmed, "{id}");
    }
    let all = world.facts.list(adk_harness::FactFilter::user(APP, "alice")).await.unwrap();
    let project_of = |text: &str| {
        all.iter().find(|fact| fact.text == text).and_then(|fact| fact.scope.project.clone())
    };
    assert_eq!(project_of("Standups have moved later.").as_deref(), Some("work"));
    assert_eq!(project_of("Evenings have got busier.").as_deref(), Some("home"));
    let texts =
        |facts: Vec<MemoryFact>| facts.into_iter().map(|fact| fact.text).collect::<Vec<_>>();
    let home_facts = texts(home.facts(&alice).await.unwrap());
    let work_facts = texts(work.facts(&alice).await.unwrap());
    assert!(!home_facts.iter().any(|fact| fact.contains("Standups")), "{home_facts:?}");
    assert!(!work_facts.iter().any(|fact| fact.contains("Evenings")), "{work_facts:?}");
    assert!(work_facts.iter().any(|fact| fact.contains("Standups")), "{work_facts:?}");
    kernel.stop().await.unwrap();
}
