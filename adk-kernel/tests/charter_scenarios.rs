//! The charter machinery on a kernel with real `DurableWorker`s and a scripted model (spec
//! 3.5 §7.10, §7.11, A-I3): a content-gap signal lands on the back office's agenda as an
//! `AgendaOrigin::Signal` item and wakes the `site.content` process sleeping on its kind; a
//! repeat counts on the same item; nothing about it is a crossing, and a back-office request
//! for the raw text behind a signal is a crossing the partition policy denies. The KPI
//! tracker runs as the kernel's `kernel.kpi` maintenance process.

#![cfg(feature = "charter")]

mod common;

use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditRecorder};
use adk_core::{GovernanceControl, PolicyDecision, RunState, ToolPolicy, ToolPolicyRequest};
use adk_kernel::charter::{
    InMemorySignalStore, SIGNAL_SOURCE_TOOL, SignalOutcome, SignalSink, SignalSourceTool,
    SiteProcessKind, audit,
};
use adk_kernel::conformance::charter::fixtures::{content_gap, emitted_by};
use adk_kernel::{
    AgendaOrigin, AgendaState, CrossPartition, CrossingFilter, CrossingOutcome, KernelEvent,
    Partition, PartitionId, Partitions, QueueClass, WakeOutcome, WakeSet,
};
use common::*;

const BACK_OFFICE: &str = "back-office";

#[tokio::test]
async fn a_signal_lands_on_the_agenda_and_wakes_the_process_sleeping_on_its_kind() {
    let world = World::answer();
    let control = GovernanceControl::new();
    let kernel = world.kernel("k", &control);
    kernel.start().await.unwrap();
    let mut events = kernel.events();
    let partition = PartitionId::new(BACK_OFFICE);

    let kind = SiteProcessKind::Content;
    let spec = kind.spec(BACK_OFFICE).with_wake(WakeSet::any_of([kind.default_wake(&partition)]));
    let sleeping = kernel
        .processes()
        .spawn(
            run_on(QueueClass::Background, "Turn the content-gap signals into draft pages"),
            spec,
        )
        .await
        .unwrap();
    let pid = sleeping.meta.pid.clone();
    in_state(&world.runs, &pid, RunState::Waiting).await;
    assert_eq!(world.model_calls(), 0, "a sleeping process makes no model call");
    let meta = kernel.processes().get(&pid).await.unwrap().unwrap().meta;
    assert_eq!(SiteProcessKind::of(&meta), Some(SiteProcessKind::Content));

    let recorder = InMemoryAuditRecorder::new();
    let sink = SignalSink::for_kernel(&kernel, Arc::new(InMemorySignalStore::new()), BACK_OFFICE)
        .with_audit(AuditTrail::new(Arc::new(recorder.clone())));
    let filed = sink.emit(content_gap("Do you open on Sundays?", 4)).await.unwrap();
    let SignalOutcome::Filed { item, count: 1 } = filed else { panic!("{filed:?}") };

    let stored = kernel.agenda().get(&item).await.unwrap().expect("the item is on the agenda");
    assert_eq!(stored.partition, partition);
    assert_eq!(stored.state, AgendaState::Proposed);
    assert_eq!(stored.origin, AgendaOrigin::Signal { kind: "content_gap".into(), count: 1 });
    assert!(stored.rationale.contains("untrusted"), "{}", stored.rationale);

    in_state(&world.runs, &pid, RunState::Completed).await;
    assert_eq!(world.model_calls(), 1, "the signal woke the sleeper for one turn");
    let woke = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let record = events.recv().await.expect("the bus stays open");
            if let KernelEvent::Wake {
                outcome: Some(WakeOutcome::Woke { pid: woken }), source, ..
            } = record.event
                && source == "signal"
            {
                break woken;
            }
        }
    })
    .await
    .expect("a signal wake within five seconds");
    assert_eq!(woke, pid);

    let repeat =
        sink.emit(emitted_by(content_gap("do you  open on sundays?", 1), "inbox")).await.unwrap();
    assert_eq!(repeat, SignalOutcome::Counted { item: item.clone(), count: 2 });
    let counted = kernel.agenda().get(&item).await.unwrap().unwrap();
    assert_eq!(counted.origin, AgendaOrigin::Signal { kind: "content_gap".into(), count: 2 });

    // Recorded as `kernel.signal`, with no text of the signal's.
    let records = recorder.records();
    let outcomes: Vec<String> = records
        .iter()
        .filter(|record| record.action == audit::SIGNAL)
        .map(|record| record.ext["outcome"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(outcomes, ["filed", "counted"]);
    let written =
        serde_json::to_string(&records.iter().map(|r| &r.ext).collect::<Vec<_>>()).unwrap();
    assert!(
        !written.to_lowercase().contains("sunday"),
        "the audit holds no signal text: {written}"
    );

    // A signal is not a crossing: the crossing ledger is untouched.
    let crossings = kernel.partitions().crossings(&CrossingFilter::default()).await.unwrap();
    assert!(crossings.is_empty(), "{crossings:?}");
    kernel.stop().await.unwrap();
}

#[tokio::test]
async fn a_source_past_its_rate_limit_files_nothing() {
    let world = World::answer();
    let kernel = world.kernel("k", &GovernanceControl::new());
    let sink = SignalSink::for_kernel(&kernel, Arc::new(InMemorySignalStore::new()), BACK_OFFICE)
        .with_rate_limit(adk_kernel::charter::SignalRateLimit::new(
            3,
            std::time::Duration::from_secs(3600),
        ));
    for topic in ["a", "b", "c"] {
        assert!(
            sink.emit(emitted_by(content_gap(topic, 1), "flooder")).await.unwrap().item().is_some()
        );
    }
    let refused = sink.emit(emitted_by(content_gap("d", 1), "flooder")).await.unwrap();
    assert_eq!(refused, SignalOutcome::RateLimited { source: "flooder".into(), count: 4 });
    assert!(sink.emit(content_gap("d", 1)).await.unwrap().item().is_some(), "per source");
    let filed = kernel
        .agenda()
        .list(&adk_kernel::AgendaFilter::default().partition(BACK_OFFICE))
        .await
        .unwrap();
    assert_eq!(filed.len(), 4);
}

#[tokio::test]
async fn a_request_for_the_raw_text_behind_a_signal_is_a_denied_crossing() {
    let partitions = Partitions::in_memory();
    let by = AuditActor::default();
    partitions.put(Partition::new("public", "public-agents"), by.clone()).await.unwrap();
    partitions
        .put(
            Partition::new(BACK_OFFICE, "back-office-agents")
                .with_cross_partition(CrossPartition::CoordinatorWithApproval),
            by,
        )
        .await
        .unwrap();
    let policy = partitions.policy(BACK_OFFICE).with_coordinator("steward");
    let tool = SignalSourceTool::new(&policy, "public");
    assert_eq!(adk_core::Tool::name(&tool), SIGNAL_SOURCE_TOOL);
    for (agent, invocation) in [("growth", "inv-1"), ("steward", "inv-2")] {
        let mut request =
            ToolPolicyRequest::new(SIGNAL_SOURCE_TOOL, serde_json::json!({ "item": "signal-1" }));
        request.agent_name = agent.into();
        request.invocation_id = invocation.into();
        let decision = policy.evaluate(&request).await;
        assert!(matches!(decision, PolicyDecision::Deny { .. }), "{agent}: {decision:?}");
    }
    let denied = partitions
        .crossings(&CrossingFilter::default().outcomes([CrossingOutcome::Denied]))
        .await
        .unwrap();
    assert_eq!(denied.len(), 2, "both requests are recorded as denied crossings");
    assert!(denied.iter().all(|record| record.to.as_str() == "public"));
}

/// A source that always reads 7.
struct Seven;

#[async_trait::async_trait]
impl adk_kernel::charter::KpiSource for Seven {
    fn name(&self) -> &str {
        "commerce"
    }

    async fn read(
        &self,
        _: uuid::Uuid,
        _: &str,
        _: chrono::DateTime<chrono::Utc>,
        _: chrono::DateTime<chrono::Utc>,
    ) -> adk_kernel::Result<Option<f64>> {
        Ok(Some(7.0))
    }
}

#[tokio::test]
async fn the_kpi_tracker_runs_as_a_kernel_maintenance_process() {
    use adk_kernel::charter::{
        CharterStore, InMemoryCharterStore, InMemoryKpiStore, KPI_TRACKER_TARGET, KpiStore,
        KpiTracker, KpiTrackerExecutor,
    };
    use adk_kernel::conformance::charter::fixtures::{charter, decision, kpi};
    use adk_kernel::{ProcessKind, ProcessSpec};

    let site = uuid::Uuid::new_v4();
    let charters = Arc::new(InMemoryCharterStore::new());
    let mut document = charter(site, &["amina"]);
    document.kpis.push(kpi("leads", "leads.qualified", "commerce"));
    charters
        .commit(document, decision("v1", chrono::Utc::now(), &[(awp_types::Role::Owner, "amina")]))
        .await
        .unwrap();
    let kpis = Arc::new(InMemoryKpiStore::new());
    let tracker = KpiTracker::new(site, charters, kpis.clone()).with_source(Arc::new(Seven));
    let trigger = tracker.trigger(APP, "0 0 3 * * *");
    assert_eq!(trigger.run.target, KPI_TRACKER_TARGET);
    let world = World::answer();
    let control = GovernanceControl::new();
    let executor = Arc::new(KpiTrackerExecutor::new([tracker]));
    let kernel = world.kernel_with("k", &control, |builder| builder.workflows(executor));
    kernel.start().await.unwrap();
    let process = kernel
        .processes()
        .spawn(trigger.run.clone(), ProcessSpec::new(ProcessKind::Maintenance))
        .await
        .unwrap();
    let done = in_state(&world.runs, &process.meta.pid, RunState::Completed).await;
    let report = done.result.expect("the tracker's report");
    assert_eq!(report["charterVersion"], 1);
    assert_eq!(report["kpis"][0]["kpi"], "leads");
    assert_eq!(report["kpis"][0]["phase"]["phase"], "measureOnly");
    let now = chrono::Utc::now();
    let samples = kpis.samples(site, "leads", now - chrono::Duration::hours(1), now).await.unwrap();
    assert_eq!(samples.iter().map(|s| s.value).collect::<Vec<_>>(), [7.0]);
    assert_eq!(world.model_calls(), 0, "the tracker makes no model call");
    kernel.stop().await.unwrap();
}
