//! The consolidation tick under a kernel (spec 3.2 §7.5, §10 HK2, K-I12, K-I22, K-I23): on a
//! clock the test controls, the tick is spawned only once the partition has new material,
//! the interval has passed and the partition has been idle for the debounce; it runs as a
//! `Maintenance` process, distils the session the per-session distiller never reached with
//! promotion off, consolidates the facts, and files its proposal as a `Consolidation` item
//! that stays `Proposed` in an Auto partition.

#![cfg(all(feature = "kernel", feature = "memory"))]

use crate::common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::InMemoryAuditRecorder;
use adk_core::{
    Content, DurableRunStore, Event, InMemoryRunStore, ManualClock, RunState, UsageMetadata, UserId,
};
use adk_harness::{
    FactKind, FactScope, FactSource, FactStatus, FactStore, Harness, HarnessAgent, HarnessMode,
    InMemoryFactStore, LongTermMemoryConfig, MemoryFact, Provenance, project_slug,
};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::{
    AgendaFilter, AgendaOrigin, AgendaState, CONSOLIDATION_LABEL, ConsolidationTick,
    InMemoryProcessStore, Kernel, KernelConfig, PartitionId, ProactivityPolicy, Process,
    ProcessFilter, ProcessKind, ProcessStore,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::background::DurableWorkerConfig;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use chrono::Utc;
use serde_json::json;

use common::APP;

const DIMENSIONS: usize = 128;

/// A unit vector on `axis`, tilted toward the next axis by `tilt`.
fn vector(axis: usize, tilt: f32) -> Vec<f32> {
    let mut vector = vec![0.0; DIMENSIONS];
    vector[axis] = 1.0;
    vector[(axis + 1) % DIMENSIONS] = tilt;
    vector
}

struct World {
    dir: tempfile::TempDir,
    store: Arc<InMemoryFactStore>,
    sessions: Arc<InMemorySessionService>,
    audit: InMemoryAuditRecorder,
    clock: ManualClock,
    runs: Arc<dyn DurableRunStore>,
}

impl World {
    fn scope(&self) -> FactScope {
        FactScope::new(APP, "alice", Some(project_slug(self.dir.path())))
    }

    /// Stores a fact learned now on the kernel's clock, which the gate compares it with.
    async fn seed(&self, text: &str, status: FactStatus, vector: Vec<f32>) -> MemoryFact {
        let mut fact = MemoryFact::new(
            self.scope(),
            FactKind::ProjectFact,
            text,
            status,
            Provenance::new(FactSource::AgentInferred, "s0", "inv-0"),
        )
        .with_confidence(0.8);
        fact.valid_from = adk_core::Clock::now(&self.clock);
        let stored = self.store.put(fact).await.unwrap();
        let embedding = adk_harness::FactEmbedding::new("test/seeded", vector);
        self.store.put_embedding(&stored.id, embedding).await.unwrap();
        stored
    }

    fn harness(&self, distiller: &Arc<ScriptedLlm>) -> Harness {
        let agent = Arc::new(
            HarnessAgent::builder("harness")
                .model(Arc::new(ScriptedLlm::new("m", Vec::<ScriptedResponse>::new())))
                .workspace(adk_devtools::Workspace::new(self.dir.path()))
                .project_instructions(common::no_user_file())
                .without_compaction()
                .mode(HarnessMode::Auto)
                .long_term_memory(LongTermMemoryConfig {
                    distill_model: Some(distiller.clone()),
                    ..LongTermMemoryConfig::default()
                })
                .fact_store(self.store.clone())
                .build()
                .unwrap(),
        );
        let runner = agent
            .configure_runner(
                Runner::builder()
                    .app_name(APP)
                    .agent(agent.clone())
                    .session_service(self.sessions.clone() as Arc<dyn SessionService>),
            )
            .audit_log(Arc::new(self.audit.clone()))
            .build()
            .unwrap();
        Harness::with_runner(agent, runner).unwrap().with_run_store(Arc::clone(&self.runs))
    }
}

async fn ticks(kernel: &Kernel) -> Vec<Process> {
    kernel
        .processes()
        .ps(ProcessFilter::default())
        .await
        .unwrap()
        .into_iter()
        .filter(|process| {
            process.meta.labels.get(CONSOLIDATION_LABEL).map(String::as_str)
                == Some("consolidation")
        })
        .collect()
}

/// Moves the kernel's clock past the next evaluation and gives the scheduler tick time to
/// run it.
async fn evaluate_after(world: &World, by: Duration) {
    world.clock.advance(by);
    tokio::time::sleep(Duration::from_millis(400)).await;
}

async fn completed(world: &World, pid: &str) {
    let started = tokio::time::Instant::now();
    while started.elapsed() < Duration::from_secs(15) {
        if let Ok(Some(run)) = world.runs.get(pid).await
            && run.state == RunState::Completed
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("tick {pid} did not complete");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_tick_runs_only_after_new_material_interval_and_debounce() {
    let start = Utc::now();
    let clock = ManualClock::new(start);
    let world = World {
        dir: tempfile::tempdir().unwrap(),
        store: Arc::new(InMemoryFactStore::new()),
        sessions: Arc::new(InMemorySessionService::new()),
        audit: InMemoryAuditRecorder::new(),
        runs: Arc::new(InMemoryRunStore::new().with_clock(Arc::new(clock.clone()))),
        clock,
    };
    let known = world
        .seed("Deploys go out from the release branch", FactStatus::Known, vector(0, 0.0))
        .await;
    let distiller = Arc::new(ScriptedLlm::new(
        "distiller",
        vec![
            ScriptedResponse::text(
                json!({ "facts": [{ "text": "Deploys go out from main", "kind": "decision",
                    "source": "user_stated", "quote": "deploys go out from main now",
                    "confidence": 0.8, "supersedes": known.id }] })
                .to_string(),
            )
            .with_usage(UsageMetadata { total_token_count: 120, ..Default::default() }),
        ],
    ));
    let harness = world.harness(&distiller);
    let alice = UserId::new("alice").unwrap();
    let kernel = harness
        .kernel()
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_clock(Arc::new(world.clock.clone()))
                .with_worker(DurableWorkerConfig::default().with_worker_id("k1"))
                .with_scheduler_tick(Duration::from_millis(50))
                .with_control(ControlPlaneConfig::default().without_socket())
                .with_proactivity(
                    ProactivityPolicy::default().with_consolidation(ConsolidationTick::default()),
                ),
        )
        .process_store(Arc::new(InMemoryProcessStore::new()) as Arc<dyn ProcessStore>)
        .consolidator(PartitionId::default(), Arc::new(harness.consolidator(&alice).unwrap()))
        .build()
        .unwrap();
    kernel.start().await.unwrap();

    // No material: one known fact is not 25 new facts.
    evaluate_after(&world, Duration::from_secs(61)).await;
    assert!(ticks(&kernel).await.is_empty(), "no new material");

    // 25 new facts, and a session the user was active in a moment ago.
    let older = world.seed("the build needs protoc", FactStatus::Assumed, vector(1, 0.0)).await;
    world.clock.advance(Duration::from_secs(1));
    let newer =
        world.seed("building needs protoc installed", FactStatus::Assumed, vector(1, 0.3)).await;
    for i in 0..23 {
        world
            .seed(&format!("unrelated fact {i}"), FactStatus::Assumed, vector(3 + 2 * i, 0.0))
            .await;
    }
    world
        .sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: "alice".into(),
            session_id: Some("s1".into()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let mut said = Event::new("inv-1");
    said.author = "user".into();
    said.timestamp = adk_core::Clock::now(&world.clock);
    said.llm_response.content =
        Some(Content::new("user").with_text("We changed it: deploys go out from main now."));
    world.sessions.append_event("s1", said).await.unwrap();
    evaluate_after(&world, Duration::from_secs(61)).await;
    assert!(ticks(&kernel).await.is_empty(), "the partition was active within the debounce");

    // Idle for the debounce: the first tick runs.
    evaluate_after(&world, Duration::from_secs(30 * 60)).await;
    let first = ticks(&kernel).await;
    assert_eq!(first.len(), 1, "one tick");
    let tick = &first[0];
    assert_eq!(
        (tick.meta.kind.clone(), tick.run.queue.as_str()),
        (ProcessKind::Maintenance, "maintenance")
    );
    completed(&world, &tick.meta.pid).await;

    let records = harness.consolidations(&alice).await.unwrap();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.tick_id, tick.meta.pid, "the tick's id is its pid");
    assert_eq!(record.sessions, vec!["s1".to_string()], "the session the distiller never reached");
    assert_eq!(
        world.store.get(&older.id).await.unwrap().unwrap().status,
        FactStatus::Superseded { by: newer.id.clone() }
    );
    assert_eq!(world.store.get(&known.id).await.unwrap().unwrap(), known, "K-I22");
    let distilled = world.store.get(&record.distilled[0]).await.unwrap().unwrap();
    assert_eq!(distilled.status, FactStatus::Assumed, "K-I12: promotion is off");
    assert_eq!(record.beside_known[0].known, known.id);
    assert_eq!(
        world.audit.records().iter().filter(|r| r.action == "harness.memory.consolidated").count(),
        1
    );

    // The proposal is a Consolidation item, Proposed in an Auto partition (K-I23).
    let items = kernel.agenda().list(&AgendaFilter::default()).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        (items[0].origin.clone(), items[0].state.clone()),
        (AgendaOrigin::Consolidation, AgendaState::Proposed)
    );
    let followups = kernel.processes().ps(ProcessFilter::default()).await.unwrap();
    assert!(followups.iter().all(|process| process.meta.kind != ProcessKind::Proposed));

    // New material again, but within the interval: no second tick until six hours pass.
    world.clock.advance(Duration::from_secs(1));
    for i in 0..25 {
        world.seed(&format!("later fact {i}"), FactStatus::Assumed, vector(64 + i, 0.0)).await;
    }
    evaluate_after(&world, Duration::from_secs(61)).await;
    assert_eq!(ticks(&kernel).await.len(), 1, "within min_interval");
    evaluate_after(&world, Duration::from_secs(6 * 60 * 60)).await;
    let all = ticks(&kernel).await;
    assert_eq!(all.len(), 2, "the interval passed");
    let second = all.iter().find(|process| process.meta.pid != tick.meta.pid).unwrap();
    completed(&world, &second.meta.pid).await;
    assert_eq!(distiller.requests().len(), 1, "s1 was not distilled twice");
    kernel.stop().await.unwrap();
}
